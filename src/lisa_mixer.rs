//! EXPERIMENT (isolated): adaptive probability *mixture* of four specialised residual models, an alternative to
//! LISA Stage 3 + Stage 4. Nothing here is used by production LISA; it consumes the public Stage 1/2 API and the PKON
//! binary range coder (16-bit probabilities).
//!
//! Residual alphabet: the zigzag residual z in 0..=2^(bd+1)-2, binarized MSB-first as a complete binary tree of depth
//! D = bd+1 (no escape, no cutoff: every representable residual has a leaf). Each model assigns every tree node a
//! probability P_i(bit=1 | node, its context), so it defines a full distribution p_i(z) = product over the path.
//!
//! Mixture: P_t(z) = sum_i w_i p_i(z). It is coded exactly, one binary decision at a time. With a_i the product of
//! model i's branch probabilities along the path so far (a_i = 1 at the root), the conditional of the mixture is
//!   P(bit=1 | path) = sum_i w_i a_i q_i(node) / sum_i w_i a_i,   q_i = P_i(bit=1 | node)
//! and after the last decision a_i = p_i(z), sum_i w_i a_i = P_t(z). The probability is quantised to 16 bits and clamped
//! to [1, 65535] for the range coder.
//!
//! Models (all use only residuals strictly before the current one; contexts are computed before coding it and the model
//! states are updated afterwards, identically in encoder and decoder):
//!   0 global   : one binary KT-style count pair per node, q = (c1+1/2)/(c0+c1+1). Counts are halved when c0+c1 reaches
//!                2^24, which none of the experiment images hit at inner nodes (stationary in practice).
//!   1 recent   : one 16-bit probability per node, p += (target-p) >> 5 (exponential forgetting). A node's effective memory
//!                is ~32 *visits to that node* (the root: the last ~32 residuals; deeper nodes: longer in wall-clock
//!                residuals, since they are visited less often). p is clamped to [64, 65472].
//!   2 spatial  : same count pairs as global, selected by a 64-way context (qW, qN): the quantised magnitudes of the
//!                left and upper residuals of the same Bayer plane, q(z) = min((bitlen(z)+1)/2, 7).
//!   3 activity : same count pairs, selected by an 18-way context min(bitlen(zW+zN+zNW+zNE), 17) (neighbours of the same
//!                plane; missing neighbours count as 0).
//! A single-stream (non-Bayer, Morton) layout has no plane geometry; neighbours then degrade to the previous 1..4 residuals
//! in coding order (W,N,NW,NE = i-1,i-2,i-3,i-4). That path is exercised only by tests, not by the evaluation.
//!
//! Weights (online exponential log-loss with forgetting):
//!   u_i = w_i * max(p_i(z), 1e-30)^eta,  w' = u / sum u,  w = (1-rho) w' + rho/M,   eta = 0.1, rho = 1/1024, M = 4.
//! Every weight is >= rho/M = 1/4096 after each update, so no model is ever excluded. Initial weights are 1/4.
//!
//! Determinism: the coding path uses f64 + - * / and `powf`, evaluated in the same order by encoder and decoder. A stream
//! is therefore reproducible by the same binary on the same platform; bit-exact portability of `powf` across libm
//! implementations is NOT claimed.
//!
//! Stream: 26-byte header (magic, version, mode, eta in 1/1000, rho denominator, residual count, payload length) +
//! range-coder payload. Geometry/bit depth/CFA are not in the stream (same accounting as the cloud and production LISA).

use crate::rangecoder::{BinDec, BinEnc, BitIo};
use crate::lisa::*;

pub const MAGIC: [u8; 4] = *b"PMIX";
pub const VERSION: u8 = 1;
pub const MODE_MIX4: u8 = 1;
pub const ETA_MILLI: u16 = 100;
pub const RHO_DEN: u16 = 1024;
pub const HEADER_LEN: usize = 4 + 1 + 1 + 2 + 2 + 8 + 8;
pub const M: usize = 4;
pub const MODEL_NAMES: [&str; M] = ["global", "recent", "spatial", "activity"];
pub const SEGMENTS: usize = 16;

const ETA: f64 = ETA_MILLI as f64 / 1000.0;
const RHO: f64 = 1.0 / RHO_DEN as f64;
const P_EPS: f64 = 1e-30;
const COUNT_LIMIT: u32 = 1 << 24;
const RECENT_SHIFT: u32 = 5;
const SP_CTX: usize = 64;
const ACT_CTX: usize = 18;
const BIG: u32 = 1024; // diagnostic only: residuals >= BIG are reported separately

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Ctx {
    pub sp: usize,
    pub act: usize,
}

/// Per-symbol diagnostic values (ideal bits), filled only when requested.
#[derive(Clone, Copy, Debug, Default)]
pub struct SymDiag {
    pub mix_bits: f64,
    pub model_bits: [f64; M],
    pub w_before: [f64; M],
}

#[derive(Clone, Debug, Default)]
pub struct Segment {
    pub n: u64,
    pub mix_bits: f64,
    pub model_bits: [f64; M],
    pub w_sum: [f64; M],
}

/// Ideal-cost accounting gathered by the encoder (bits).
#[derive(Clone, Debug, Default)]
pub struct MixStats {
    pub n: u64,
    pub mix_bits: f64,
    pub model_bits: [f64; M],
    /// residuals with z >= 1024: count and ideal mixture bits spent on them
    pub big_n: u64,
    pub big_bits: f64,
    pub final_weights: [f64; M],
    pub segments: Vec<Segment>,
}

pub struct Mixer {
    depth: usize,
    nodes: usize,
    pub w: [f64; M],
    global: Vec<[u32; 2]>,
    recent: Vec<u16>,
    spatial: Vec<[u32; 2]>,
    activity: Vec<[u32; 2]>,
}

#[inline]
fn kt(c: [u32; 2]) -> f64 {
    (c[1] as f64 + 0.5) / (c[0] as f64 + c[1] as f64 + 1.0)
}

#[inline]
fn bump(c: &mut [u32; 2], bit: u32) {
    c[bit as usize] += 1;
    if c[0] + c[1] >= COUNT_LIMIT {
        c[0] = (c[0] + 1) >> 1;
        c[1] = (c[1] + 1) >> 1;
    }
}

/// Weight update (see module docs). `a[i]` is model i's probability of the observed residual.
pub fn update_weights(w: &mut [f64; M], a: &[f64; M]) {
    let mut u = [0f64; M];
    let mut s = 0f64;
    for i in 0..M {
        u[i] = w[i] * a[i].max(P_EPS).powf(ETA);
        s += u[i];
    }
    for i in 0..M {
        w[i] = (1.0 - RHO) * (u[i] / s) + RHO / M as f64;
    }
}

impl Mixer {
    pub fn new(depth: usize) -> Self {
        let nodes = 1usize << depth;
        Mixer {
            depth,
            nodes,
            w: [1.0 / M as f64; M],
            global: vec![[0; 2]; nodes],
            recent: vec![32768; nodes],
            spatial: vec![[0; 2]; SP_CTX * nodes],
            activity: vec![[0; 2]; ACT_CTX * nodes],
        }
    }

    /// Each model's P(bit = 1) at `node` under context `c`.
    #[inline]
    pub fn q_at(&self, node: usize, c: Ctx) -> [f64; M] {
        [
            kt(self.global[node]),
            self.recent[node] as f64 / 65536.0,
            kt(self.spatial[c.sp * self.nodes + node]),
            kt(self.activity[c.act * self.nodes + node]),
        ]
    }

    /// Codes one residual (tree symbol `sym` in 0..2^depth). Encoders pass the symbol; decoders pass 0 and receive it.
    pub fn code<I: BitIo>(&mut self, io: &mut I, sym: usize, c: Ctx, diag: Option<&mut SymDiag>) -> usize {
        let d = self.depth;
        let mut a = [1f64; M];
        let mut node = 1usize;
        for lvl in 0..d {
            let q = self.q_at(node, c);
            let (mut num, mut den) = (0f64, 0f64);
            for i in 0..M {
                let t = self.w[i] * a[i];
                den += t;
                num += t * q[i];
            }
            let p1 = ((num / den) * 65536.0 + 0.5) as i64;
            let p1 = p1.clamp(1, 65535) as u32;
            let bit = io.code(p1, ((sym >> (d - 1 - lvl)) & 1) as u32);
            for i in 0..M {
                a[i] *= if bit == 1 { q[i] } else { 1.0 - q[i] };
            }
            node = 2 * node + bit as usize;
        }
        let leaf = node - self.nodes;

        if let Some(dg) = diag {
            let pm: f64 = (0..M).map(|i| self.w[i] * a[i]).sum();
            dg.mix_bits = -pm.log2();
            for i in 0..M {
                dg.model_bits[i] = -a[i].max(P_EPS).log2();
            }
            dg.w_before = self.w;
        }

        // updates happen only after the whole symbol is coded
        for lvl in 0..d {
            let n = (1usize << lvl) | (leaf >> (d - lvl));
            let bit = ((leaf >> (d - 1 - lvl)) & 1) as u32;
            bump(&mut self.global[n], bit);
            let r = &mut self.recent[n];
            let target = (bit as i32) << 16;
            *r = (*r as i32 + ((target - *r as i32) >> RECENT_SHIFT)).clamp(64, 65536 - 64) as u16;
            bump(&mut self.spatial[c.sp * self.nodes + n], bit);
            bump(&mut self.activity[c.act * self.nodes + n], bit);
        }
        update_weights(&mut self.w, &a);
        leaf
    }
}

#[inline]
fn quant_mag(z: u32) -> usize {
    (((32 - z.leading_zeros()) as usize + 1) / 2).min(7)
}

/// Causal context of position `i` of stream `s` (only s[..i] is read). `pw` = plane width, 0 = linear fallback.
pub fn ctx_of(s: &[u32], i: usize, pw: usize) -> Ctx {
    let g = |k: usize| if i >= k { s[i - k] } else { 0 };
    let (w, n, nw, ne) = if pw == 0 {
        (g(1), g(2), g(3), g(4))
    } else {
        let (x, y) = (i % pw, i / pw);
        (
            if x > 0 { s[i - 1] } else { 0 },
            if y > 0 { s[i - pw] } else { 0 },
            if x > 0 && y > 0 { s[i - pw - 1] } else { 0 },
            if y > 0 && x + 1 < pw { s[i - pw + 1] } else { 0 },
        )
    };
    let sum = w as u64 + n as u64 + nw as u64 + ne as u64;
    Ctx { sp: quant_mag(w) * 8 + quant_mag(n), act: ((64 - sum.leading_zeros()) as usize).min(ACT_CTX - 1) }
}

/// (residuals, plane width or 0) per stream in Stage 2 order.
fn streams(data: &OrganizedResiduals, width: usize) -> Vec<(&[u32], usize)> {
    match data {
        OrganizedResiduals::Morton(r) => vec![(r.as_slice(), 0)],
        OrganizedResiduals::BayerPlanes(p) => p.iter().enumerate().map(|(i, v)| (v.as_slice(), (width + 1 - (i & 1)) / 2)).collect(),
    }
}

fn depth_of(bd: u8) -> Result<usize, LisaError> {
    if (1..=16).contains(&bd) { Ok(bd as usize + 1) } else { Err(LisaError::UnsupportedBitDepth(bd)) }
}

/// Encodes the Stage 2 residuals. Returns (stream, header length, ideal-cost stats).
pub fn encode_mixer(stage2: &Stage2Output) -> Result<(Vec<u8>, usize, MixStats), LisaError> {
    let bd = stage2.bit_depth();
    let depth = depth_of(bd)?;
    let zmax = (2u64 << bd) - 2;
    let width = stage2.metadata().width;
    let st = streams(stage2.data(), width);
    let total: u64 = st.iter().map(|(s, _)| s.len() as u64).sum();
    let mut mixer = Mixer::new(depth);
    let mut enc = BinEnc::new();
    let mut stats = MixStats { segments: vec![Segment::default(); SEGMENTS], ..Default::default() };
    let mut dg = SymDiag::default();
    for (s, pw) in st {
        for i in 0..s.len() {
            let z = s[i];
            if u64::from(z) > zmax {
                return Err(LisaError::CorruptStream("residual outside the bit-depth range"));
            }
            let c = ctx_of(s, i, pw);
            mixer.code(&mut enc, z as usize, c, Some(&mut dg));
            let seg = &mut stats.segments[(stats.n * SEGMENTS as u64 / total) as usize];
            seg.n += 1;
            seg.mix_bits += dg.mix_bits;
            stats.mix_bits += dg.mix_bits;
            for k in 0..M {
                seg.model_bits[k] += dg.model_bits[k];
                seg.w_sum[k] += dg.w_before[k];
                stats.model_bits[k] += dg.model_bits[k];
            }
            if z >= BIG {
                stats.big_n += 1;
                stats.big_bits += dg.mix_bits;
            }
            stats.n += 1;
        }
    }
    stats.final_weights = mixer.w;
    let payload = enc.finish();
    let mut out = Vec::with_capacity(HEADER_LEN + payload.len());
    out.extend_from_slice(&MAGIC);
    out.push(VERSION);
    out.push(MODE_MIX4);
    out.extend_from_slice(&ETA_MILLI.to_le_bytes());
    out.extend_from_slice(&RHO_DEN.to_le_bytes());
    out.extend_from_slice(&stats.n.to_le_bytes());
    out.extend_from_slice(&(payload.len() as u64).to_le_bytes());
    out.extend_from_slice(&payload);
    Ok((out, HEADER_LEN, stats))
}

/// Decodes a mixer stream to Stage 1 residuals (Morton order). `metadata` and `bit_depth` are supplied externally.
pub fn decode_mixer(stream: &[u8], metadata: &ImageMetadata, bit_depth: u8) -> Result<Stage1Output, LisaError> {
    if stream.len() < HEADER_LEN {
        return Err(LisaError::CorruptStream("mixer stream shorter than header"));
    }
    if stream[..4] != MAGIC {
        return Err(LisaError::CorruptStream("bad mixer magic"));
    }
    if stream[4] != VERSION || stream[5] != MODE_MIX4 {
        return Err(LisaError::CorruptStream("unsupported mixer version or mode"));
    }
    if u16::from_le_bytes([stream[6], stream[7]]) != ETA_MILLI || u16::from_le_bytes([stream[8], stream[9]]) != RHO_DEN {
        return Err(LisaError::CorruptStream("unsupported mixer parameters"));
    }
    let count = u64::from_le_bytes(stream[10..18].try_into().unwrap());
    let plen = u64::from_le_bytes(stream[18..26].try_into().unwrap());
    metadata.validate()?;
    let n = validate(metadata.width, metadata.height, bit_depth)?;
    let depth = depth_of(bit_depth)?;
    if count != n as u64 {
        return Err(LisaError::CorruptStream("residual count disagrees with geometry"));
    }
    if plen != (stream.len() - HEADER_LEN) as u64 {
        return Err(LisaError::CorruptStream("payload length disagrees with stream length"));
    }
    let payload = &stream[HEADER_LEN..];
    let zmax = (2u64 << bit_depth) - 2;

    // Layout template: organize the identity index sequence; tells where every Morton index lands in Stage 2 order.
    let idx = Stage1Output { residuals: (0..n as u32).collect(), metadata: metadata.clone(), bit_depth };
    let tmpl = organize_stage2(idx)?;

    let mut mixer = Mixer::new(depth);
    let mut dec = BinDec::new(payload);
    let mut residuals = vec![0u32; n];
    for (order, pw) in streams(tmpl.data(), metadata.width) {
        let mut cur: Vec<u32> = Vec::with_capacity(order.len());
        for &morton_idx in order {
            let c = ctx_of(&cur, cur.len(), pw);
            let z = mixer.code(&mut dec, 0, c, None) as u32;
            if u64::from(z) > zmax {
                return Err(LisaError::CorruptStream("residual outside the bit-depth range"));
            }
            cur.push(z);
            residuals[morton_idx as usize] = z;
        }
    }
    if dec.consumed() != payload.len() {
        return Err(LisaError::CorruptStream("payload truncated or has trailing bytes"));
    }
    Ok(Stage1Output { residuals, metadata: metadata.clone(), bit_depth })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lcg(seed: &mut u32) -> u32 {
        *seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
        *seed >> 8
    }

    fn image(w: usize, h: usize, bits: u8) -> Vec<u16> {
        let maxv = (1u32 << bits) - 1;
        let mut s = 777u32;
        (0..w * h)
            .map(|i| {
                let (x, y) = (i % w, i / w);
                let amp = match (x * 4 / w, y * 2 / h) {
                    (0, _) => 0,
                    (1, _) => 3,
                    (2, _) => 24,
                    _ => maxv,
                };
                ((maxv / 2) as i64 + (lcg(&mut s) as i64 % (amp as i64 + 1)) - amp as i64 / 2).clamp(0, maxv as i64) as u16
            })
            .collect()
    }

    fn bayer(w: usize, h: usize) -> ImageMetadata {
        let cfa = CfaPattern {
            width: 2,
            height: 2,
            entries: vec![ColorFilter::Red, ColorFilter::Green, ColorFilter::Green, ColorFilter::Blue],
            origin_x: 0,
            origin_y: 0,
        };
        ImageMetadata::with_cfa(w, h, SampleLayout::BayerCfa, cfa)
    }

    fn pipeline(samples: &[u16], meta: &ImageMetadata, bits: u8) -> (Stage2Output, Vec<u8>, usize, MixStats) {
        let s1 = encode_with_metadata(samples, meta.width, meta.height, bits, meta.clone()).unwrap();
        let s2 = organize_stage2(s1).unwrap();
        let (stream, hdr, stats) = encode_mixer(&s2).unwrap();
        (s2, stream, hdr, stats)
    }

    fn roundtrip(samples: &[u16], meta: ImageMetadata, bits: u8) -> usize {
        let (_s2, stream, _hdr, stats) = pipeline(samples, &meta, bits);
        let s1b = decode_mixer(&stream, &meta, bits).unwrap();
        assert_eq!(s1b.metadata, meta);
        assert_eq!(decode_with_metadata(&s1b).unwrap(), samples);
        // determinism: re-encoding gives the identical stream
        let (_, again, _, _) = pipeline(samples, &meta, bits);
        assert_eq!(again, stream);
        assert_eq!(stats.n, (meta.width * meta.height) as u64);
        stream.len()
    }

    #[test]
    fn exact_roundtrip_bayer_mono_and_edges() {
        roundtrip(&image(23, 17, 12), bayer(23, 17), 12); // odd dims -> uneven planes
        roundtrip(&image(19, 14, 10), ImageMetadata::without_cfa(19, 14, SampleLayout::Monochrome), 10); // linear fallback
        roundtrip(&image(2, 2, 8), bayer(2, 2), 8);
        roundtrip(&image(3, 3, 8), bayer(3, 3), 8);
        roundtrip(&image(1, 1, 8), ImageMetadata::without_cfa(1, 1, SampleLayout::Monochrome), 8);
        roundtrip(&image(16, 12, 16), bayer(16, 12), 16); // full-range 16-bit residuals
        roundtrip(&vec![0u16; 8 * 8], bayer(8, 8), 12);
        roundtrip(&vec![4095u16; 8 * 8], bayer(8, 8), 12);
    }

    #[test]
    fn large_residuals_round_trip_without_cutoff() {
        // alternate extremes -> residuals near the top of the range
        let s: Vec<u16> = (0..16 * 16).map(|i| if (i / 3) % 2 == 0 { 0 } else { 65535 }).collect();
        roundtrip(&s, bayer(16, 16), 16);
    }

    #[test]
    fn mixture_is_a_normalised_positive_distribution() {
        let depth = 6;
        let mut m = Mixer::new(depth);
        let mut enc = BinEnc::new();
        let mut seed = 5u32;
        let mut ctx = Ctx::default();
        for t in 0..3000 {
            let sym = if t % 7 == 0 { (lcg(&mut seed) % 64) as usize } else { (lcg(&mut seed) % 5) as usize };
            ctx = Ctx { sp: (t % SP_CTX), act: t % ACT_CTX };
            m.code(&mut enc, sym, ctx, None);
        }
        for cc in [ctx, Ctx { sp: 3, act: 0 }, Ctx { sp: 63, act: 17 }] {
            let mut total = 0f64;
            let mut per_model = [0f64; M];
            for sym in 0..(1usize << depth) {
                let mut a = [1f64; M];
                let mut node = 1usize;
                for lvl in 0..depth {
                    let q = m.q_at(node, cc);
                    let bit = (sym >> (depth - 1 - lvl)) & 1;
                    for i in 0..M {
                        a[i] *= if bit == 1 { q[i] } else { 1.0 - q[i] };
                    }
                    node = 2 * node + bit;
                }
                let p: f64 = (0..M).map(|i| m.w[i] * a[i]).sum();
                assert!(p > 0.0, "zero-probability symbol {sym}");
                total += p;
                for i in 0..M {
                    assert!(a[i] > 0.0);
                    per_model[i] += a[i];
                }
            }
            assert!((total - 1.0).abs() < 1e-9, "mixture sums to {total}");
            for s in per_model {
                assert!((s - 1.0).abs() < 1e-9, "model sums to {s}");
            }
        }
    }

    #[test]
    fn weights_stay_normalised_deterministic_and_floored() {
        let mut w1 = [0.25; M];
        let mut w2 = [0.25; M];
        for t in 0..10_000 {
            let a = [1e-3 * (1 + t % 3) as f64, 0.2, 1e-40, 0.05];
            update_weights(&mut w1, &a);
            update_weights(&mut w2, &a);
            assert_eq!(w1, w2);
            assert!((w1.iter().sum::<f64>() - 1.0).abs() < 1e-12);
            assert!(w1.iter().all(|&x| x >= RHO / M as f64 * 0.999999 && x.is_finite()));
        }
    }

    #[test]
    fn better_model_gains_weight() {
        let mut w = [0.25; M];
        let mut prev = w[1];
        for _ in 0..200 {
            update_weights(&mut w, &[0.01, 0.2, 0.01, 0.01]);
            assert!(w[1] >= prev - 1e-15);
            prev = w[1];
        }
        assert!(w[1] > 0.9 && w[0] < w[1] && w[2] < w[1] && w[3] < w[1], "{w:?}");
        // a single update with equal weights follows the formula
        let mut w = [0.25; M];
        update_weights(&mut w, &[0.5, 0.25, 0.25, 0.25]);
        let u: Vec<f64> = [0.5f64, 0.25, 0.25, 0.25].iter().map(|p| 0.25 * p.powf(0.1)).collect();
        let s: f64 = u.iter().sum();
        for i in 0..M {
            assert!((w[i] - ((1.0 - RHO) * u[i] / s + RHO / 4.0)).abs() < 1e-15);
        }
    }

    #[test]
    fn recovers_after_distribution_change() {
        let depth = 6;
        let mut m = Mixer::new(depth);
        let mut enc = BinEnc::new();
        let mut seed = 99u32;
        let mut dg = SymDiag::default();
        let (mut mix_last, mut glob_last) = (0f64, 0f64);
        let (na, nb, tail) = (6000, 6000, 1000);
        for t in 0..(na + nb) {
            let sym = if t < na { lcg(&mut seed) % 8 } else { 40 + lcg(&mut seed) % 8 } as usize;
            m.code(&mut enc, sym, Ctx::default(), Some(&mut dg));
            if t >= na + nb - tail {
                mix_last += dg.mix_bits;
                glob_last += dg.model_bits[0];
            }
        }
        let (mix, glob) = (mix_last / tail as f64, glob_last / tail as f64);
        assert!(mix < 3.6, "mixture {mix} bits/sym after the change (entropy 3.0)");
        assert!(mix < glob - 0.3, "mixture {mix} should beat the stale global model {glob}");
        assert!(m.w[1] > m.w[0], "recent model should outweigh global after the change: {:?}", m.w);
    }

    #[test]
    fn malformed_streams_are_rejected() {
        let (w, h) = (23, 17);
        let meta = bayer(w, h);
        let (_s2, stream, hdr, _) = pipeline(&image(w, h, 12), &meta, 12);
        let plen = stream.len() - hdr;
        let dec = |s: &[u8]| decode_mixer(s, &meta, 12);
        assert!(dec(&stream).is_ok());
        let mut c = stream.clone();
        c.truncate(stream.len() - 1);
        assert!(dec(&c).is_err(), "truncated by 1, header unchanged");
        let mut c = stream.clone();
        c.truncate(hdr + plen / 2);
        c[18..26].copy_from_slice(&((plen / 2) as u64).to_le_bytes());
        assert!(dec(&c).is_err(), "halved payload, header fixed up");
        let mut c = stream.clone();
        c[0] ^= 0xFF;
        assert!(dec(&c).is_err(), "bad magic");
        let mut c = stream.clone();
        c[5] = 2;
        assert!(dec(&c).is_err(), "bad mode");
        let mut c = stream.clone();
        c[6] ^= 1;
        assert!(dec(&c).is_err(), "bad eta");
        let mut c = stream.clone();
        c[10] ^= 1;
        assert!(dec(&c).is_err(), "bad count");
        assert!(dec(&stream[..10]).is_err(), "shorter than header");
        let mut c = stream.clone();
        c.push(0);
        assert!(dec(&c).is_err(), "trailing byte");
        assert!(decode_mixer(&stream, &meta, 8).is_err(), "wrong bit depth");
    }
}
