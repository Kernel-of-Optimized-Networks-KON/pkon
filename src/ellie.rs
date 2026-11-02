//! ELLIE: lossless image compression, one source file, three production stages.
//!
//! Stage 1: prediction and residual generation. Stage 2: context-mixing probability model for the Stage 1 residuals
//! ([`Stage3`]). Stage 3: binary range coder fed by the Stage 2 probability stream ([`Stage4Enc`], [`Stage4Dec`],
//! [`EllieCodec`]). Type names keep their historical numbering (`Stage3`, `Stage4*`). The former adaptive bias-correction
//! stage ([`Stage2`]) is no longer part of the codec; the type is retained only so historical benchmarks still compile.
//!
//! # Stage 1 specification (as approved)
//!
//! Samples are `x in [0, M]`, `M = 2^B - 1`, `1 <= B <= 16`. Planes are processed independently.
//! Eight-neighbour causal window: W, WW, N, NN, NW, NE, NNW, NNE.
//!
//! Predictors, all scaled by 4 (fixed horizontal/vertical coefficients 5/4 and -1/4):
//! ```text
//! P_H4 = 5*W - WW
//! P_V4 = 5*N - NN
//! P_D4 = (4-f)*NW + f*NE,   f = 2 + [s135 >= 2*s45] - [s45 >= 2*s135]   (f in 1..=3)
//! ```
//! Shared absolute-difference planes (each computed once per pixel, see `[H, V, A, G]` rows):
//! ```text
//! H_r[x] = |I[r][x]   - I[r][x-1]|     V_r[x] = |I[r][x]   - I[r-1][x]|
//! A_r[x] = |I[r][x-1] - I[r-1][x]|     G_r[x] = |I[r][x]   - I[r-1][x-1]|
//! ```
//! Activity measures (c = current row, p = previous row):
//! ```text
//! S_H  = H_c[x-1] + H_p[x] + H_p[x+1]       S_V  = V_c[x-1] + V_p[x] + V_p[x+1]
//! s45  = A_c[x] + A_p[x]                    s135 = G_p[x+1] + G_p[x]
//! m = min(s45, s135);  S_D = m + (m >> 2)
//! ```
//! Weights are normalized inverse squares with a common power-of-two rescale to 10 bits and a +1 floor:
//! ```text
//! sh   = max(0, bitlen(max(S_H,S_V,S_D)) - 10);   S'_i = (S_i >> sh) + 1;   q_i = S'_i^2
//! w_H ~ q_V*q_D,  w_V ~ q_H*q_D,  w_D ~ q_H*q_V;  D = sum of the three products
//! P_H4 = 4W + ((W-WW) + (N-NW) >> 1);  P_V4 = 4N + ((N-NN) + (W-NW) >> 1);
//! P_D4 = 3*((4-f)NW + f*NE)/4 + (W + NE - N)       (f in 1..3 from s45 vs s135)
//! N' = sum_i pair_i * (P_i4 + 4M);   t = N' / D;   P = clamp(((t + 2) >> 2) - M, 0, M)
//! ```
//! Residual: signed `e = x - P`; [`fold`]/[`unfold`] give the range-aware bijection onto `[0, M]`.
//!
//! # Boundaries
//! * Row 0: pixel 0 is predicted as `2^(B-1)`; later pixels as `clamp((5W - WW + 2) >> 2)` with `WW := W` at x = 1.
//! * Rows >= 1: left pads (W, WW, NW at x = 0) equal N; after pixel 0 the left pads become `I[y][0]`.
//!   Right pads replicate the last pixel of the row above. Row -1 is a replica of row 0 (affects row 1 only).
//!
//! # Integer bounds (B = 16, the worst case)
//! * S_H, S_V <= 3M < 2^18, S_D <= 2.5M; `S' <= 2^10`, `q <= 2^20`, pair products `<= 2^40`, `D < 2^42`.
//! * `P_i4 + 4M` lies in `[3M, 9M]`, `9M < 2^20`, so `N' < 3 * 2^40 * 2^20 < 2^62` fits `u64`.
//! * `t = N'/D <= 9M`, so `(t + 2) >> 2` is at most `2.25M + 1`; the final value is clamped to `[0, M]`.

/// Largest supported bit depth.
pub const MAX_BITS: u32 = 16;

use crate::rangecoder::BitIo;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage1Error {
    /// Bit depth outside `1..=MAX_BITS`.
    BadBits(u32),
    /// Slice length does not equal `width * height`.
    BadLength,
    /// An input sample exceeds `2^bits - 1`.
    SampleOutOfRange,
    /// A residual reconstructs to a sample outside `[0, 2^bits - 1]` (corrupt stream).
    ResidualOutOfRange,
}

/// Range-aware fold of the signed residual `e = x - p` onto `[0, m]` (bijective for fixed `p`).
#[inline(always)]
pub fn fold(e: i32, p: u32, m: u32) -> u32 {
    let near = p.min(m - p) as i32;
    let a = e.abs();
    let short = 2 * a - (e < 0) as i32;
    let long = a + near;
    (if a <= near { short } else { long }) as u32
}

/// Inverse of [`fold`] for `u <= m`.
#[inline(always)]
pub fn unfold(u: u32, p: u32, m: u32) -> i32 {
    let near = p.min(m - p);
    if u <= 2 * near {
        let h = (u >> 1) as i32;
        if u & 1 == 1 { -(((u + 1) >> 1) as i32) } else { h }
    } else {
        let a = (u - near) as i32;
        if p <= m - p { a } else { -a }
    }
}

/// Standard Paeth selection among A = W, B = N, C = NW (ties resolve A, then B, then C).
#[inline(always)]
fn paeth(a: i32, b: i32, c: i32) -> i32 {
    let p = a + b - c;
    let (pa, pb, pc) = ((p - a).abs(), (p - b).abs(), (p - c).abs());
    if pa <= pb && pa <= pc {
        a
    } else if pb <= pc {
        b
    } else {
        c
    }
}

/// Mixed prediction from the eight-neighbour-derived quantities. Exact integer arithmetic, one division. Each directional
/// weight is also scaled by the adaptive factor `f` (see [`Shrub`]). Returns the prediction and the 4x H/V/D estimates.
#[inline(always)]
fn predict(m: i32, w: i32, ww: i32, n: i32, nn: i32, nw: i32, ne: i32, s_h: u32, s_v: u32, s45: u32, s135: u32, fa: [u64; 3]) -> (i32, [u64; 3]) {
    let mn = s45.min(s135);
    let s_d = mn + (mn >> 2);
    let smax = s_h.max(s_v).max(s_d);
    let sh = (32 - (smax | 1).leading_zeros()).saturating_sub(10);
    let sq = |s: u32| {
        let r = ((s >> sh) + 1) as u64;
        r * r
    };
    let (qh, qv, qd) = (sq(s_h), sq(s_v), sq(s_d));
    let (ph, pv, pd) = ((qv * qd) as u128 * fa[0] as u128, (qh * qd) as u128 * fa[1] as u128, (qh * qv) as u128 * fa[2] as u128);
    let d = ph + pv + pd;

    let f = 2 + (s135 >= 2 * s45) as i32 - (s45 >= 2 * s135) as i32;
    let o = 4 * m;
    // Four times each directional estimate, plus the offset `o` that keeps the sum non-negative. H and V extrapolate
    // by the mean of the gradient on this row and the one beside it; D blends the NW/NE mix with the W+NE-N plane.
    let h4 = (4 * w + ((w - ww + n - nw) >> 1) + o) as u64;
    let v4 = (4 * n + ((n - nn + w - nw) >> 1) + o) as u64;
    // Refined by the Paeth pick of (W, N, NW): rounded average of the two estimates. Both are >= 0 (each carries `o`).
    let d4 = (((3 * ((4 - f) * nw + f * ne)) / 4 + (w + ne - n) + o + 4 * paeth(w, n, nw) + o + 1) >> 1) as u64;

    let t = ((ph * h4 as u128 + pv * v4 as u128 + pd * d4 as u128) / d) as u64;
    ((((t + 2) >> 2) as i64 - m as i64).clamp(0, m as i64) as i32, [h4, v4, d4])
}

/// Adaptive H/V/D weighting: per-estimator running mean of |4x - 4*estimate| (fixed point x16, rate 1/8), updated only
/// once the reconstructed sample is known to both sides. Factor f_i = 256 * ((Emin + 16) / (E_i + 16))^2, floored at 1.
#[derive(Clone, Copy)]
struct Shrub {
    e: [i64; 3],
}

impl Shrub {
    #[inline(always)]
    fn factors(&self) -> [u64; 3] {
        let mn = self.e[0].min(self.e[1]).min(self.e[2]);
        // Squared ratio: Q16 r = (Emin+16)/(E+16) in (0,1], r^2 in Q16, rounded to the 256 scale.
        let f = |e: i64| {
            let r = (((mn + 16) as u64) << 16) / (e + 16) as u64;
            ((256 * ((r * r) >> 16) + 32768) >> 16).max(1)
        };
        [f(self.e[0]), f(self.e[1]), f(self.e[2])]
    }

    #[inline(always)]
    fn update(&mut self, x: i32, est4: [u64; 3], m: i32) {
        let t = 4 * x as i64 + 4 * m as i64;
        for k in 0..3 {
            let err = (t - est4[k] as i64).abs() << 4;
            self.e[k] += (err - self.e[k]) >> 3;
        }
    }
}

/// Structural leaf (0..[`TREE_LEAVES`]) of the sample being coded, from causal data only. This is the `TM-nostat` configuration of
/// `benchmarks/ellie-structural-tree/` (stationarity test off). `r` = `2|e_W| + 2|e_N| + |e_NW| + |e_NE|` of the Stage 1 residual magnitudes.
///   0/1 flat (all of W,N,NW,NE equal) with quiet/active residuals; 2/3 two distinct values, same split; 5 smooth (S_H+S_V < 16);
///   otherwise 6 + 2 * predictability (r small / medium / large against S_H+S_V) + 1 if one direction clearly dominates. Leaf 4 is unused.
#[inline(always)]
pub fn tree_leaf(w: i32, n: i32, nw: i32, ne: i32, s_h: u32, s_v: u32, s_d: u32, r: u32) -> usize {
    let g = s_h + s_v;
    let d = 1 + (n != w) as u32 + (nw != w && nw != n) as u32 + (ne != w && ne != n && ne != nw) as u32;
    if d == 1 {
        return (r > 0) as usize;
    }
    if d == 2 {
        return 2 + (r > 0) as usize;
    }
    if g < 16 {
        return 5;
    }
    let p = if 4 * r < g { 0 } else if r >= g { 2 } else { 1 };
    let m = s_h.min(s_v).min(s_d);
    let med = s_h + s_v + s_d - m - s_h.max(s_v).max(s_d);
    6 + p * 2 + (med > 0 && 2 * m <= med) as usize
}

/// Reusable Stage 1 scanner for one plane geometry. All buffers are allocated once in [`Stage1::new`].
pub struct Stage1 {
    w: usize,
    h: usize,
    bits: u32,
    m: u32,
    // Three padded pixel rows (2 pad columns each side), rotated cur -> p -> pp.
    ra: Vec<u16>,
    rb: Vec<u16>,
    rc: Vec<u16>,
    // Two difference rows `[H, V, A, G]`, index = column, plus one ghost column at `w`.
    da: Vec<[u16; 4]>,
    db: Vec<[u16; 4]>,
    // Residual-magnitude rows for the structural tree (`scan_tree` only), index = column + 1.
    ma: Vec<u32>,
    mb: Vec<u32>,
}

impl Stage1 {
    pub fn new(width: usize, height: usize, bits: u32) -> Result<Self, Stage1Error> {
        if !(1..=MAX_BITS).contains(&bits) {
            return Err(Stage1Error::BadBits(bits));
        }
        Ok(Self {
            w: width,
            h: height,
            bits,
            m: (1u32 << bits) - 1,
            ra: vec![0; width + 4],
            rb: vec![0; width + 4],
            rc: vec![0; width + 4],
            da: vec![[0; 4]; width + 1],
            db: vec![[0; 4]; width + 1],
            ma: vec![0; width + 2],
            mb: vec![0; width + 2],
        })
    }

    pub fn max_value(&self) -> u32 {
        self.m
    }

    /// Residuals `res[i] = src[i] - P[i]` (signed, `P` clamped to `[0, M]`).
    pub fn encode(&mut self, src: &[u16], res: &mut [i32]) -> Result<(), Stage1Error> {
        self.check_encode(src, res.len())?;
        self.scan(|i, p| {
            let x = src[i];
            res[i] = x as i32 - p as i32;
            x as u32
        });
        Ok(())
    }

    /// Like [`Stage1::encode`] but also exports the prediction plane for later stages.
    pub fn encode_with_pred(&mut self, src: &[u16], res: &mut [i32], pred: &mut [u16]) -> Result<(), Stage1Error> {
        self.check_encode(src, res.len())?;
        if pred.len() != src.len() {
            return Err(Stage1Error::BadLength);
        }
        self.scan(|i, p| {
            let x = src[i];
            res[i] = x as i32 - p as i32;
            pred[i] = p as u16;
            x as u32
        });
        Ok(())
    }

    /// Exact inverse of [`Stage1::encode`].
    pub fn decode(&mut self, res: &[i32], out: &mut [u16]) -> Result<(), Stage1Error> {
        let n = self.w * self.h;
        if res.len() != n || out.len() != n {
            return Err(Stage1Error::BadLength);
        }
        let m = self.m as i32;
        let mut bad = false;
        self.scan(|i, p| {
            let v = p as i32 + res[i];
            bad |= (v as u32) > m as u32;
            let v = v.clamp(0, m);
            out[i] = v as u16;
            v as u32
        });
        if bad { Err(Stage1Error::ResidualOutOfRange) } else { Ok(()) }
    }

    /// Encoder for the `Bc255S11Tree` profile: residuals plus the structural leaf of every sample.
    pub fn encode_tree(&mut self, src: &[u16], res: &mut [i32], leaf: &mut [u8]) -> Result<(), Stage1Error> {
        self.check_encode(src, res.len())?;
        if leaf.len() != src.len() {
            return Err(Stage1Error::BadLength);
        }
        self.scan_tree(|i, p, l| {
            res[i] = src[i] as i32 - p as i32;
            leaf[i] = l as u8;
            src[i] as u32
        });
        Ok(())
    }

    /// Fused decoder for the `Bc255S11Tree` profile: each residual is decoded by `s3` under the leaf computed from the samples
    /// reconstructed so far. `s3` must be a `Bc255S11Tree` model of the same geometry, freshly reset.
    pub fn decode_tree<I: BitIo>(&mut self, s3: &mut Stage3, io: &mut I, out: &mut [u16]) -> Result<(), Stage1Error> {
        if out.len() != self.w * self.h {
            return Err(Stage1Error::BadLength);
        }
        let m = self.m as i32;
        let mut bad = false;
        self.scan_tree(|i, p, l| {
            let v = p as i32 + s3.code_sample_leaf(io, 0, l);
            bad |= (v as u32) > m as u32;
            let v = v.clamp(0, m);
            out[i] = v as u16;
            v as u32
        });
        if bad { Err(Stage1Error::ResidualOutOfRange) } else { Ok(()) }
    }

    /// [`Stage1::decode_tree`] for a model with the flank stage attached: `bases[i]` is the flank context base of sample `i` (from
    /// already-decoded planes) and every decoded residual is also written to `res_out` for the planes that follow.
    pub fn decode_tree_flank<I: BitIo>(&mut self, s3: &mut Stage3, io: &mut I, out: &mut [u16], res_out: &mut [i32], bases: &[u8]) -> Result<(), Stage1Error> {
        if out.len() != self.w * self.h || res_out.len() != out.len() || bases.len() != out.len() {
            return Err(Stage1Error::BadLength);
        }
        let m = self.m as i32;
        let mut bad = false;
        self.scan_tree(|i, p, l| {
            s3.set_flank_base(bases[i] as usize);
            let r = s3.code_sample_leaf(io, 0, l);
            res_out[i] = r;
            let v = p as i32 + r;
            bad |= (v as u32) > m as u32;
            let v = v.clamp(0, m);
            out[i] = v as u16;
            v as u32
        });
        if bad { Err(Stage1Error::ResidualOutOfRange) } else { Ok(()) }
    }

    fn check_encode(&self, src: &[u16], res_len: usize) -> Result<(), Stage1Error> {
        let n = self.w * self.h;
        if src.len() != n || res_len != n {
            return Err(Stage1Error::BadLength);
        }
        if src.iter().fold(0u16, |a, &s| a.max(s)) as u32 > self.m {
            return Err(Stage1Error::SampleOutOfRange);
        }
        Ok(())
    }

    /// Shared raster scan. `resolve(index, prediction)` returns the actual sample (known to the encoder,
    /// reconstructed by the decoder); everything else is identical, so both sides cannot diverge.
    #[inline(always)]
    fn scan<F: FnMut(usize, u32) -> u32>(&mut self, mut resolve: F) {
        let (w, h, m) = (self.w, self.h, self.m as i32);
        if w == 0 || h == 0 {
            return;
        }
        let mid = 1u32 << (self.bits - 1);
        let mut shrub = Shrub { e: [0; 3] };
        let mut cur: &mut [u16] = &mut self.ra[..];
        let mut p: &mut [u16] = &mut self.rb[..];
        let mut pp: &mut [u16] = &mut self.rc[..];
        let mut dcur: &mut [[u16; 4]] = &mut self.da[..];
        let mut dprev: &mut [[u16; 4]] = &mut self.db[..];

        // ---- Row 0: scalar horizontal loop; pixel 0 from mid-range. ----
        let x0 = resolve(0, mid) as u16;
        cur[2] = x0;
        cur[0] = x0;
        cur[1] = x0;
        for x in 1..w {
            let wv = cur[x + 1] as i32;
            let wwv = cur[x] as i32; // x = 1 reads the pad, which equals W
            let pr = ((5 * wv - wwv + 2) >> 2).clamp(0, m);
            cur[x + 2] = resolve(x, pr as u32) as u16;
        }
        let last = cur[w + 1];
        cur[w + 2] = last;
        cur[w + 3] = last;
        // Difference row of row 0 against a replicated row -1: V = 0, A = G = H.
        dcur[0] = [0; 4];
        for x in 1..w {
            let hh = (cur[x + 2] as i32 - cur[x + 1] as i32).unsigned_abs() as u16;
            dcur[x] = [hh, 0, hh, hh];
        }
        dcur[w] = [0; 4];
        pp.copy_from_slice(cur);
        std::mem::swap(&mut p, &mut cur); // p = row 0, cur = scratch
        std::mem::swap(&mut dprev, &mut dcur);

        // ---- Rows >= 1. ----
        for y in 1..h {
            let base = y * w;
            let n0 = p[2];
            cur[0] = n0;
            cur[1] = n0;

            // x = 0: W = WW = NW = N.
            let (mut h_c, mut v_c, mut a_c);
            {
                let n = n0 as i32;
                let (nn, ne) = (pp[2] as i32, p[3] as i32);
                let (d0, d1) = (dprev[0], dprev[1]);
                let s_h = d0[0] as u32 + d1[0] as u32;
                let s_v = d0[1] as u32 + d1[1] as u32;
                let s45 = d0[2] as u32;
                let s135 = d1[3] as u32 + d0[3] as u32;
                let (pr, est) = predict(m, n, n, n, nn, n, ne, s_h, s_v, s45, s135, shrub.factors());
                let xv = resolve(base, pr as u32);
                shrub.update(xv as i32, est, m);
                let v = (xv as i32 - n).unsigned_abs() as u16;
                cur[2] = xv as u16;
                cur[0] = xv as u16;
                cur[1] = xv as u16;
                dcur[0] = [0, v, v, v];
                h_c = 0u32;
                v_c = v as u32;
                a_c = (xv as i32 - ne).unsigned_abs();
            }

            for x in 1..w {
                let i = x + 2;
                let wv = cur[i - 1] as i32;
                let wwv = cur[i - 2] as i32;
                let n = p[i] as i32;
                let nw = p[i - 1] as i32;
                let ne = p[i + 1] as i32;
                let nn = pp[i] as i32;
                let (d0, d1) = (dprev[x], dprev[x + 1]);
                let s_h = h_c + d0[0] as u32 + d1[0] as u32;
                let s_v = v_c + d0[1] as u32 + d1[1] as u32;
                let s45 = a_c + d0[2] as u32;
                let s135 = d1[3] as u32 + d0[3] as u32;
                let (pr, est) = predict(m, wv, wwv, n, nn, nw, ne, s_h, s_v, s45, s135, shrub.factors());
                let xv = resolve(base + x, pr as u32) as i32;
                shrub.update(xv, est, m);
                let hh = (xv - wv).unsigned_abs();
                let vv = (xv - n).unsigned_abs();
                let gg = (xv - nw).unsigned_abs();
                let an = (xv - ne).unsigned_abs();
                dcur[x] = [hh as u16, vv as u16, a_c as u16, gg as u16];
                cur[i] = xv as u16;
                h_c = hh;
                v_c = vv;
                a_c = an;
            }

            // Ghost column and right pads (replicate the last pixel).
            let last = cur[w + 1];
            cur[w + 2] = last;
            cur[w + 3] = last;
            dcur[w] = [0, v_c as u16, 0, v_c as u16];

            // Rotate (pp, p, cur) <- (p, cur, pp).
            std::mem::swap(&mut pp, &mut p);
            std::mem::swap(&mut p, &mut cur);
            std::mem::swap(&mut dprev, &mut dcur);
        }
    }

    /// [`Stage1::scan`] with the structural leaf: `resolve(index, prediction, leaf)`. The prediction does not depend on the leaf (one
    /// Shrub state), so the arithmetic is identical to `scan`; the leaf is only handed to the probability model. Row 0 is leaf 0.
    #[inline(always)]
    fn scan_tree<F: FnMut(usize, u32, usize) -> u32>(&mut self, mut resolve: F) {
        let (w, h, m) = (self.w, self.h, self.m as i32);
        if w == 0 || h == 0 {
            return;
        }
        let mid = 1u32 << (self.bits - 1);
        let mut shrub = Shrub { e: [0; 3] };
        let mut cur: &mut [u16] = &mut self.ra[..];
        let mut p: &mut [u16] = &mut self.rb[..];
        let mut pp: &mut [u16] = &mut self.rc[..];
        let mut dcur: &mut [[u16; 4]] = &mut self.da[..];
        let mut dprev: &mut [[u16; 4]] = &mut self.db[..];
        let mut am_c: &mut [u32] = &mut self.ma[..];
        let mut am_p: &mut [u32] = &mut self.mb[..];
        am_p.fill(0);

        // ---- Row 0: as `scan`, plus residual magnitudes. ----
        am_c[0] = am_p[1];
        let x0 = resolve(0, mid, 0) as u16;
        cur[2] = x0;
        cur[0] = x0;
        cur[1] = x0;
        am_c[1] = (x0 as i32 - mid as i32).unsigned_abs();
        for x in 1..w {
            let wv = cur[x + 1] as i32;
            let wwv = cur[x] as i32;
            let pr = ((5 * wv - wwv + 2) >> 2).clamp(0, m);
            let v = resolve(x, pr as u32, 0);
            cur[x + 2] = v as u16;
            am_c[x + 1] = (v as i32 - pr).unsigned_abs();
        }
        let last = cur[w + 1];
        cur[w + 2] = last;
        cur[w + 3] = last;
        dcur[0] = [0; 4];
        for x in 1..w {
            let hh = (cur[x + 2] as i32 - cur[x + 1] as i32).unsigned_abs() as u16;
            dcur[x] = [hh, 0, hh, hh];
        }
        dcur[w] = [0; 4];
        pp.copy_from_slice(cur);
        std::mem::swap(&mut p, &mut cur);
        std::mem::swap(&mut dprev, &mut dcur);
        std::mem::swap(&mut am_p, &mut am_c);
        am_p[0] = am_p[1];
        am_p[w + 1] = am_p[w];

        // ---- Rows >= 1. ----
        for y in 1..h {
            let base = y * w;
            let n0 = p[2];
            cur[0] = n0;
            cur[1] = n0;
            am_c[0] = am_p[1];
            let (mut h_c, mut v_c, mut a_c) = (0u32, 0u32, 0u32);
            for x in 0..w {
                let i = x + 2;
                let (wv, wwv, nw) = if x == 0 { (n0 as i32, n0 as i32, n0 as i32) } else { (cur[i - 1] as i32, cur[i - 2] as i32, p[i - 1] as i32) };
                let n = p[i] as i32;
                let ne = p[i + 1] as i32;
                let nn = pp[i] as i32;
                let (d0, d1) = (dprev[x], dprev[x + 1]);
                let s_h = h_c + d0[0] as u32 + d1[0] as u32;
                let s_v = v_c + d0[1] as u32 + d1[1] as u32;
                let s45 = a_c + d0[2] as u32;
                let s135 = d1[3] as u32 + d0[3] as u32;
                let mn = s45.min(s135);
                let j = x + 1;
                let r = 2 * am_c[j - 1] + 2 * am_p[j] + am_p[j - 1] + am_p[j + 1];
                let leaf = tree_leaf(wv, n, nw, ne, s_h, s_v, mn + (mn >> 2), r);
                let (pr, est) = predict(m, wv, wwv, n, nn, nw, ne, s_h, s_v, s45, s135, shrub.factors());
                let xv = resolve(base + x, pr as u32, leaf) as i32;
                shrub.update(xv, est, m);
                am_c[x + 1] = (xv - pr).unsigned_abs();
                if x == 0 {
                    let v = (xv - n).unsigned_abs() as u16;
                    cur[2] = xv as u16;
                    cur[0] = xv as u16;
                    cur[1] = xv as u16;
                    dcur[0] = [0, v, v, v];
                    h_c = 0;
                    v_c = v as u32;
                    a_c = (xv - ne).unsigned_abs();
                } else {
                    let hh = (xv - wv).unsigned_abs();
                    let vv = (xv - n).unsigned_abs();
                    let gg = (xv - nw).unsigned_abs();
                    let an = (xv - ne).unsigned_abs();
                    dcur[x] = [hh as u16, vv as u16, a_c as u16, gg as u16];
                    cur[i] = xv as u16;
                    h_c = hh;
                    v_c = vv;
                    a_c = an;
                }
            }
            let last = cur[w + 1];
            cur[w + 2] = last;
            cur[w + 3] = last;
            dcur[w] = [0, v_c as u16, 0, v_c as u16];
            std::mem::swap(&mut pp, &mut p);
            std::mem::swap(&mut p, &mut cur);
            std::mem::swap(&mut dprev, &mut dcur);
            std::mem::swap(&mut am_p, &mut am_c);
            am_p[0] = am_p[1];
            am_p[w + 1] = am_p[w];
        }
    }
}

// ---------------------------------------------------------------------------------------------------
// Retired: adaptive bias tracking (not executed by the production codec; kept for historical benchmarks)
//
// One fixed-point (S2_FRAC fractional bits) running mean of the signed residual e. With x_i = e_i << F:
//   B_i       = (S_i + 2^(F-1)) >> F                  (rounded integer bias)
//   r_i       = e_i - B_i                             (corrected residual)
//   S_{i+1}   = S_i + ((x_i - S_i) >> K)              (K = 6)
// S is a mean of e, so |S| <= M<<F and |B| <= M: no clamp is needed and r = e - B stays in [-2M, 2M].
// Every update uses e_i, which the decoder recovers as r_i + B_i, so both sides evolve identically with no
// side information. State: one i32 per plane, initialised to 0.
// ---------------------------------------------------------------------------------------------------

/// Fractional bits of the bias state.
pub const S2_FRAC: u32 = 8;
/// Adaptation rate: the bias moves by 1/2^S2_SHIFT of its error each sample.
pub const S2_SHIFT: u32 = 6;

/// Retired adaptive bias state for one plane; not used by [`EllieCodec`]. Create one per plane (or call [`Stage2::reset`]).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Stage2 {
    s: i32,
}

impl Stage2 {
    pub const fn new() -> Self {
        Self { s: 0 }
    }

    pub fn reset(&mut self) {
        *self = Self::new();
    }

    /// Raw fixed-point bias state (for verification).
    pub fn state(&self) -> i32 {
        self.s
    }

    /// Current integer bias `B`.
    #[inline(always)]
    pub fn bias(&self) -> i32 {
        (self.s + (1 << (S2_FRAC - 1))) >> S2_FRAC
    }

    #[inline(always)]
    fn update(&mut self, e: i32) {
        self.s += ((e << S2_FRAC) - self.s) >> S2_SHIFT;
    }

    /// Encode one residual: returns `r = e - B` and advances the state.
    #[inline(always)]
    pub fn encode_one(&mut self, e: i32) -> i32 {
        let r = e - self.bias();
        self.update(e);
        r
    }

    /// Decode one residual: returns `e = r + B` and advances the state identically.
    #[inline(always)]
    pub fn decode_one(&mut self, r: i32) -> i32 {
        let e = r + self.bias();
        self.update(e);
        e
    }

    // The in-place loops carry T = S + 2^(F-1) instead of S, so B = T >> F is a single shift and the update needs no
    // separate rounding add. Exact rewrite of the definition above (|T| < 2^25, no overflow):
    //   encode: S += ((e << F) - S) >> K            ==  T += (((e << F) + 2^(F-1)) - T) >> K
    //   decode: (e << F) - S, with e = r + (T >> F), ==  (r << F) + 2^(F-1) - (T & (2^F - 1))
    // because (T >> F) << F = T - (T & (2^F - 1)) for arithmetic shifts. The decode update therefore depends on r,
    // not on the freshly decoded e, which shortens the loop-carried chain from 7 dependent ops to 4.

    /// In-place encode of a residual plane (Stage 1 output -> bias-corrected).
    pub fn encode_in_place(&mut self, buf: &mut [i32]) {
        const HALF: i32 = 1 << (S2_FRAC - 1);
        let mut t = self.s + HALF;
        for v in buf.iter_mut() {
            let e = *v;
            *v = e - (t >> S2_FRAC);
            t += (((e << S2_FRAC) + HALF) - t) >> S2_SHIFT;
        }
        self.s = t - HALF;
    }

    /// In-place decode of a residual plane (bias-corrected -> Stage 1 input).
    pub fn decode_in_place(&mut self, buf: &mut [i32]) {
        const HALF: i32 = 1 << (S2_FRAC - 1);
        const MASK: i32 = (1 << S2_FRAC) - 1;
        let mut t = self.s + HALF;
        for v in buf.iter_mut() {
            let r = *v;
            *v = r + (t >> S2_FRAC);
            t += (((r << S2_FRAC) + HALF) - (t & MASK)) >> S2_SHIFT;
        }
        self.s = t - HALF;
    }
}

// ---------------------------------------------------------------------------------------------------
// Stage 3: context-mixing probability model
//
// Symbol representation of r in [-2M, 2M] (a = |r|, k = bitlen(a), kmax = bitlen(2M)):
//   node 0        : a != 0
//   nodes 1..k    : unary exponent, node j codes (k > j), skipped at j = kmax
//   nodes 17..32  : first mantissa bit below the leading one (k >= 2), node 15 + k
//   nodes 33..62  : second mantissa bit (k >= 3), node 33 + 2(k-3) + first bit
//   remaining k-3 mantissa bits: raw, p = 1/2 (not modelled; the density is nearly flat inside a bucket)
//   sign (a != 0): own adaptive probability, context = (sign W, sign N) in {-,0,+}^2
// Every modelled bit mixes four predictions, each an adaptive probability for the same node under:
//   A: qS                        (coarse neighbourhood energy)
//   B: qS x qd                   (energy x |aW - aN| disagreement)
//   C: qE x qW                   (long-run energy x west magnitude)
//   D: node only                 (order-0 root shared by everything)
// with S = 2aW + 2aN + aNW + aNE, E += ((a<<4) - E) >> 4, q(v) = 2*bitlen(v) + next-bit - 2 for v >= 4 else v.
// Slot (u32) = p22 << 10 | n; update p += (bit*(2^22-1) - p) * RECIP[n] >> 16, RECIP[n] = 2^17/(2n+3), n <= LIMIT.
// Mixer: x = clamp(sum w_i * in_i >> 16, +-2047), p = squash(x) (12 bit); in_i = stretch(p_i) * CONF[min(n_i,4)] >> 3
// (unseen slots contribute 0), bias input 256; w_i += (in_i * ((bit << 12) - p)) >> MIX_SHIFT; weight set =
// (qS >> 3, node). Output to the coder: P(bit = 1) = 16*p + 8 clamped to [32, 65503] (the coder's 16-bit scale).
// ---------------------------------------------------------------------------------------------------

const S3_LIMIT: u32 = 127;
const S3_MIX_SHIFT: u32 = 10;
// Profile `Bc255S11`: same model with only contexts B and C mixed (A and D contribute nothing), count cap 255, mixer shift 11.
const S3_LIMIT_BC: u32 = 255;
const S3_MIX_SHIFT_BC: u32 = 11;
const S3_W_CLAMP: i32 = 1 << 20;
const S3_IN: usize = 5;
const NQ: usize = 32; // qS / qE levels
const ND: usize = 6; // qd levels
const NW: usize = 8; // qW levels
const OFF_A: usize = 0;
const OFF_B: usize = OFF_A + NQ * 64;
const OFF_C: usize = OFF_B + NQ * ND * 64;
const OFF_D: usize = OFF_C + NQ * NW * 64;
const S3_SLOTS: usize = OFF_D + 64;
// Profile `Bc255S11Tree`: one extra mixed context, structural leaf x qS, in the input slot that `Bc255S11` leaves idle.
pub const TREE_LEAVES: usize = 12;
const S3_SLOTS_TREE: usize = S3_SLOTS + TREE_LEAVES * NQ * 64;
const SLOT_INIT: u32 = 1 << 31; // p = 1/2, n = 0
const CONF: [i32; 5] = [0, 4, 6, 7, 8];

// ---------------------------------------------------------------------------------------------------
// Cross-plane flank stage (ULISA, cross-plane context version 1)
//
// Adds one input to the `Bc255S11Tree` mixer: the adaptive probability of the decision (`node`, 0..=62) under the flank context, in the
// input slot (3) that the tree profile otherwise leaves idle. The flank context is the signed level of the sum of already-coded
// residuals at the mosaic-flank sites of the other parity planes (`FlankTopology`) crossed with the local activity level qa from this
// plane's W and N residual magnitudes:
//   ctx = ((sign(sum) * 10 + min(bitlen(|sum|), 9)) * FL_QA + qa),  sign in {0: negative, 1: zero, 2: positive}, qa = min((bitlen(aW + aN) + 1) / 2, FL_QA - 1)
// The sign bit has no mixer (it is coded from its own adaptive probability), so it gets a two-input refinement instead: the sign model's
// stretch and the flank slot's stretch (plus a bias) are mixed by per-node weights and squashed (node 63). Everything is integer; the final
// P(bit = 1) is 16 p + 8 clamped to [COD_P_MIN, COD_P_MAX] exactly like the tree mixer, so the coder's probability clamp is unchanged.
// The flank state (slots, sign-stage weights) is shared by the four planes of an image and starts fresh for every image.
// ---------------------------------------------------------------------------------------------------

/// Clamp of the 16-bit probability handed to the range coder by the tree mixer and the flank stage: `P(bit = 1)` is always in
/// `[COD_P_MIN, COD_P_MAX] / 65536`, so one decision costs at least `-log2(COD_P_MAX / 65536)` bits.
pub const COD_P_MIN: i32 = 32;
pub const COD_P_MAX: i32 = 65535 - 32;

/// Cross-plane context version written to and required from ULISA streams.
pub const FLANK_CTX_VERSION: u8 = 1;
const FL_QA: usize = 7;
const FL_SIGNLEVEL: usize = 30;
/// Flank contexts per node.
pub const FL_CTX: usize = FL_SIGNLEVEL * FL_QA;
const FL_NODES: usize = 64;
/// Decision node of the sign bit (mixer nodes are 0..=62).
const FL_SIGN_NODE: usize = 63;
const FL_BIAS: i32 = 77;
const FL_SHIFT: u32 = 13;

/// Adaptive state of the flank stage.
#[derive(Clone, PartialEq, Eq, Default)]
pub struct FlankModel {
    q: Vec<u32>,
    w: Vec<[i32; 3]>,
}

impl FlankModel {
    fn empty() -> Self {
        Self { q: Vec::new(), w: Vec::new() }
    }
    /// Fresh state: every probability slot at p = 1/2 (unseen), every weight set at (1, 0, 0).
    pub fn new() -> Self {
        Self { q: vec![SLOT_INIT; FL_NODES * FL_CTX], w: vec![[1 << 16, 0, 0]; FL_NODES] }
    }

    /// Forward pass: refine the stretch-domain probability `x` of decision `node` under flank context `fctx`. Returns the refined
    /// 12-bit probability, the flank slot index and its stretch (both needed by [`FlankModel::update`]).
    #[inline(always)]
    fn mix(&self, node: usize, fctx: usize, x: i32) -> (i32, usize, i32) {
        debug_assert!(node < FL_NODES && fctx < FL_CTX);
        let fq = node * FL_CTX + fctx;
        let fs = STRETCH[(self.q[fq] >> 20) as usize] as i32;
        let fw = self.w[node];
        let z = ((x as i64 * fw[0] as i64 + fs as i64 * fw[1] as i64 + FL_BIAS as i64 * fw[2] as i64) >> 16).clamp(-2047, 2047) as i32;
        (SQUASH[(z + 2048) as usize] as i32, fq, fs)
    }

    #[inline(always)]
    fn update(&mut self, node: usize, x: i32, fq: usize, fs: i32, p12o: i32, bit: u32) {
        let err2 = ((bit as i32) << 12) - p12o;
        let fw = &mut self.w[node];
        // Round to nearest: a flooring shift leans negative on every small update and drifts the weights.
        const RND: i32 = 1 << (FL_SHIFT - 1);
        fw[0] = (fw[0] + ((x * err2 + RND) >> FL_SHIFT)).clamp(-S3_W_CLAMP, S3_W_CLAMP);
        fw[1] = (fw[1] + ((fs * err2 + RND) >> FL_SHIFT)).clamp(-S3_W_CLAMP, S3_W_CLAMP);
        fw[2] = (fw[2] + ((FL_BIAS * err2 + RND) >> FL_SHIFT)).clamp(-S3_W_CLAMP, S3_W_CLAMP);
        s3_update_slot(&mut self.q[fq], bit, S3_LIMIT_BC);
    }
}

/// Context base for a flank sum: `(sign * 10 + min(bitlen(|sum|), 9)) * FL_QA`.
#[inline(always)]
pub fn flank_base(sum: i32) -> usize {
    let sg = if sum > 0 { 2 } else if sum < 0 { 0 } else { 1 };
    let lvl = ((32 - sum.unsigned_abs().leading_zeros()) as usize).min(9);
    (sg * 10 + lvl) * FL_QA
}

const RECIP: [u32; 256] = {
    let mut t = [0u32; 256];
    let mut n = 0;
    while n < 256 {
        t[n] = 131072 / (2 * n as u32 + 3);
        n += 1;
    }
    t
};

const fn squash_fn(d: i32) -> i32 {
    const T: [i32; 33] = [
        1, 2, 3, 6, 10, 16, 27, 45, 73, 120, 194, 310, 488, 747, 1101, 1546, 2047, 2549, 2994, 3348, 3607, 3785, 3901, 3975, 4022, 4050,
        4068, 4079, 4085, 4089, 4092, 4093, 4094,
    ];
    if d > 2047 {
        return 4095;
    }
    if d < -2047 {
        return 0;
    }
    let w = d & 127;
    let i = ((d >> 7) + 16) as usize;
    (T[i] * (128 - w) + T[i + 1] * w + 64) >> 7
}

/// squash over x in [-2048, 2047], index x + 2048.
static SQUASH: [u16; 4096] = {
    let mut t = [0u16; 4096];
    let mut i = 0;
    while i < 4096 {
        t[i] = squash_fn(i as i32 - 2048) as u16;
        i += 1;
    }
    t
};

/// Inverse of squash: stretch(p) for 12-bit p.
static STRETCH: [i16; 4096] = {
    let mut t = [2047i16; 4096];
    let mut pi = 0usize;
    let mut x = -2047i32;
    while x <= 2047 {
        let v = squash_fn(x) as usize;
        while pi <= v {
            t[pi] = x as i16;
            pi += 1;
        }
        x += 1;
    }
    t
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage3Error {
    /// Bit depth outside `1..=MAX_BITS`.
    BadBits(u32),
    /// Slice length does not equal `width * height`.
    BadLength,
    /// `|r| > 2M`.
    ResidualOutOfRange,
    /// The `Bc255S11Tree` profile needs a per-sample leaf; use the `*_leaf` entry points.
    LeafRequired,
}

/// Quantiser: identity below 4, then two levels per octave.
#[inline(always)]
fn s3q(v: u32) -> usize {
    if v < 4 {
        v as usize
    } else {
        let bl = 32 - v.leading_zeros();
        (2 * bl + ((v >> (bl - 2)) & 1) - 2) as usize
    }
}

#[inline(always)]
fn s3_update_slot(s: &mut u32, bit: u32, limit: u32) {
    let n = *s & 1023;
    let p = (*s >> 10) as i64;
    let target = if bit != 0 { (1i64 << 22) - 1 } else { 0 };
    // n <= limit <= 255, so the mask is a no-op that lets the compiler drop the bounds check.
    let p = p + (((target - p) * RECIP[(n & 255) as usize] as i64) >> 16);
    *s = ((p as u32) << 10) | (n + (n < limit) as u32);
}

#[inline(always)]
fn s3_input(s: u32) -> i32 {
    let n = ((s & 1023) as usize).min(4);
    (STRETCH[(s >> 20) as usize] as i32 * CONF[n]) >> 3
}

/// Stage 3 configuration plus pipeline shape. Each profile is a distinct bitstream; the container records which one a file was written with.
/// `Legacy4` and `Bc255S11` are the four-stage pipelines (Stage 1, bias correction [`Stage2`], model, coder); `Bc255S11Direct` is the
/// current three-stage pipeline (Stage 1, model, coder).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stage3Profile {
    /// Original model: contexts A, B, C, D mixed; count cap 127; mixer shift 10. Used by files written before profiles existed.
    Legacy4,
    /// Contexts B and C only; count cap 255; mixer shift 11 (sign slots use the same cap). Same for every plane.
    Bc255S11,
    /// The `Bc255S11` model fed directly by Stage 1 residuals, with no bias correction.
    Bc255S11Direct,
    /// `Bc255S11Direct` plus the structural tree (see [`tree_leaf`]): the leaf selects an extra qS-conditioned probability context and the
    /// mixer weight set. The leaf depends on reconstructed pixels, so the decoder fuses Stage 1 and Stage 3 ([`Stage1::decode_tree`]).
    Bc255S11Tree,
}

/// Stage 3 model for one plane of `width x height` residuals, coded in raster order.
/// One generic path ([`Stage3::code_sample`]) serves encoder and decoder, so contexts, probabilities and updates
/// cannot diverge. All storage is allocated in [`Stage3::new`].
#[derive(Clone, PartialEq, Eq)]
pub struct Stage3 {
    w: usize,
    h: usize,
    kmax: u32,
    rmax: u32,
    x: usize,
    e: i32,
    prev_m: Vec<u32>,
    cur_m: Vec<u32>,
    prev_s: Vec<u8>,
    cur_s: Vec<u8>,
    t: Vec<u32>,
    sg: [u32; 9],
    wt: Vec<[i32; S3_IN]>,
    profile: Stage3Profile,
    /// Cross-plane flank stage (see [`FlankModel`]); active only when `fl_on`. Not touched by [`Stage3::reset`].
    fl: FlankModel,
    fl_on: bool,
    /// Flank context base (`sign/level * FL_QA`) for the sample about to be coded; set by the caller per sample.
    fbase: usize,
    /// Full flank context (`fbase + qa`) of the sample being coded.
    fctx: usize,
}

impl Stage3 {
    /// Model with the [`Stage3Profile::Legacy4`] profile.
    pub fn new(width: usize, height: usize, bits: u32) -> Result<Self, Stage3Error> {
        Self::with_profile(width, height, bits, Stage3Profile::Legacy4)
    }

    pub fn with_profile(width: usize, height: usize, bits: u32, profile: Stage3Profile) -> Result<Self, Stage3Error> {
        if !(1..=MAX_BITS).contains(&bits) {
            return Err(Stage3Error::BadBits(bits));
        }
        let rmax = 2 * ((1u32 << bits) - 1);
        let mut s = Self {
            w: width,
            h: height,
            kmax: 32 - rmax.leading_zeros(),
            rmax,
            x: 0,
            e: 0,
            prev_m: vec![0; width + 2],
            cur_m: vec![0; width + 2],
            prev_s: vec![1; width + 2],
            cur_s: vec![1; width + 2],
            t: vec![SLOT_INIT; if profile == Stage3Profile::Bc255S11Tree { S3_SLOTS_TREE } else { S3_SLOTS }],
            sg: [SLOT_INIT; 9],
            wt: vec![[0; S3_IN]; if profile == Stage3Profile::Bc255S11Tree { TREE_LEAVES } else { 4 } * 64],
            profile,
            fl: FlankModel::empty(),
            fl_on: false,
            fbase: 0,
            fctx: 0,
        };
        s.reset();
        Ok(s)
    }

    /// Return to the initial state (reuse for the next plane of the same geometry).
    pub fn reset(&mut self) {
        self.x = 0;
        self.e = 0;
        self.prev_m.fill(0);
        self.cur_m.fill(0);
        self.prev_s.fill(1);
        self.cur_s.fill(1);
        self.t.fill(SLOT_INIT);
        self.sg = [SLOT_INIT; 9];
        self.wt.fill([20000, 20000, 20000, 20000, 0]);
    }

    /// Bytes of adaptive model state (probability slots + sign slots + mixer weights).
    pub fn model_state_bytes(&self) -> usize {
        self.t.len() * 4 + self.sg.len() * 4 + self.wt.len() * S3_IN * 4
    }

    /// Bytes of all heap storage owned by the model (model state + neighbour rows).
    pub fn total_bytes(&self) -> usize {
        self.model_state_bytes() + (self.prev_m.len() + self.cur_m.len()) * 4 + self.prev_s.len() + self.cur_s.len()
    }

    /// Encode a whole plane of Stage 1 residuals through `io`.
    pub fn encode_plane<I: BitIo>(&mut self, res: &[i32], io: &mut I) -> Result<(), Stage3Error> {
        if res.len() != self.w * self.h {
            return Err(Stage3Error::BadLength);
        }
        let lim = self.rmax;
        if res.iter().any(|&r| r.unsigned_abs() > lim) {
            return Err(Stage3Error::ResidualOutOfRange);
        }
        match self.profile {
            Stage3Profile::Legacy4 => {
                for &r in res {
                    self.code_sample_g::<I, false, false, false>(io, r, 0);
                }
            }
            Stage3Profile::Bc255S11 | Stage3Profile::Bc255S11Direct => {
                for &r in res {
                    self.code_sample_g::<I, true, false, false>(io, r, 0);
                }
            }
            Stage3Profile::Bc255S11Tree => return Err(Stage3Error::LeafRequired),
        }
        Ok(())
    }

    /// [`Stage3::encode_plane`] for the `Bc255S11Tree` profile: `leaf[i] < TREE_LEAVES` is the structural leaf of sample `i`.
    pub fn encode_plane_leaf<I: BitIo>(&mut self, res: &[i32], leaf: &[u8], io: &mut I) -> Result<(), Stage3Error> {
        if res.len() != self.w * self.h || leaf.len() != res.len() {
            return Err(Stage3Error::BadLength);
        }
        let lim = self.rmax;
        if res.iter().any(|&r| r.unsigned_abs() > lim) {
            return Err(Stage3Error::ResidualOutOfRange);
        }
        assert!(self.profile == Stage3Profile::Bc255S11Tree, "encode_plane_leaf needs Stage3Profile::Bc255S11Tree");
        for (&r, &l) in res.iter().zip(leaf) {
            self.code_sample_leaf(io, r, l as usize);
        }
        Ok(())
    }

    /// Decode a whole plane. Values are bounded by `2^kmax - 1` (>= 2M); callers validate via Stage 1.
    pub fn decode_plane<I: BitIo>(&mut self, out: &mut [i32], io: &mut I) -> Result<(), Stage3Error> {
        if out.len() != self.w * self.h {
            return Err(Stage3Error::BadLength);
        }
        match self.profile {
            Stage3Profile::Legacy4 => {
                for o in out.iter_mut() {
                    *o = self.code_sample_g::<I, false, false, false>(io, 0, 0);
                }
            }
            Stage3Profile::Bc255S11 | Stage3Profile::Bc255S11Direct => {
                for o in out.iter_mut() {
                    *o = self.code_sample_g::<I, true, false, false>(io, 0, 0);
                }
            }
            Stage3Profile::Bc255S11Tree => return Err(Stage3Error::LeafRequired),
        }
        Ok(())
    }

    /// Attach the cross-plane flank stage to a `Bc255S11Tree` model. From now on [`Stage3::code_sample_leaf`] runs the flank stage and the
    /// caller must call [`Stage3::set_flank_base`] before every sample. Panics for any other profile.
    pub fn attach_flank(&mut self, m: FlankModel) {
        assert!(self.profile == Stage3Profile::Bc255S11Tree, "the flank stage needs the Bc255S11Tree profile");
        assert!(m.q.len() == FL_NODES * FL_CTX && m.w.len() == FL_NODES, "flank model not initialised");
        self.fl = m;
        self.fl_on = true;
    }

    /// Detach and return the flank state (to carry it to the next plane's model).
    pub fn detach_flank(&mut self) -> FlankModel {
        self.fl_on = false;
        std::mem::take(&mut self.fl)
    }

    /// Flank context base ([`flank_base`]) of the next sample.
    #[inline(always)]
    pub fn set_flank_base(&mut self, base: usize) {
        debug_assert!(base + FL_QA <= FL_CTX);
        self.fbase = base;
    }

    /// [`Stage3::encode_plane_leaf`] with the flank stage attached: `bases[i]` is the flank context base ([`flank_base`]) of sample `i`,
    /// derived from residuals of already-coded planes only.
    pub fn encode_plane_leaf_flank<I: BitIo>(&mut self, res: &[i32], leaf: &[u8], io: &mut I, bases: &[u8]) -> Result<(), Stage3Error> {
        if res.len() != self.w * self.h || leaf.len() != res.len() || bases.len() != res.len() {
            return Err(Stage3Error::BadLength);
        }
        if res.iter().any(|&r| r.unsigned_abs() > self.rmax) {
            return Err(Stage3Error::ResidualOutOfRange);
        }
        assert!(self.fl_on, "attach_flank first");
        for ((&r, &l), &b) in res.iter().zip(leaf).zip(bases) {
            self.fbase = b as usize;
            self.code_sample_leaf(io, r, l as usize);
        }
        Ok(())
    }

    /// The sign decision under the flank stage: the sign model's own probability `ps` (16-bit) is the base prediction. Node 63.
    #[inline(always)]
    fn code_sign_flank<I: BitIo>(&mut self, io: &mut I, ps: u32, want: u32) -> u32 {
        let x = STRETCH[(ps >> 4) as usize] as i32;
        let (p12o, fq, fs) = self.fl.mix(FL_SIGN_NODE, self.fctx, x);
        let bit = io.code((p12o * 16 + 8).clamp(COD_P_MIN, COD_P_MAX) as u32, want);
        self.fl.update(FL_SIGN_NODE, x, fq, fs, p12o, bit);
        bit
    }

    #[inline(always)]
    fn code_bit<I: BitIo, const BC: bool, const TREE: bool, const FL: bool>(&mut self, io: &mut I, idx: [usize; 3], wb: usize, node: usize, want: u32) -> u32 {
        let (ia, ib, ic) = (idx[0] + node, idx[1] + node, idx[2] + node);
        let id = OFF_D + node;
        // Bounds: node <= 62; ia/ib/ic/id < S3_SLOTS by the qS/qd/qE/qW caps; wb + node < 256 (wb <= 192).
        debug_assert!(ia < self.t.len() && ib < S3_SLOTS && ic < S3_SLOTS && id < S3_SLOTS && wb + node < self.wt.len());
        let (limit, shift) = if BC { (S3_LIMIT_BC, S3_MIX_SHIFT_BC) } else { (S3_LIMIT, S3_MIX_SHIFT) };
        let (t, wt) = (&mut self.t[..], &mut self.wt[..]);
        // SAFETY: indices are within the allocations (see above). Under BC the A and D inputs are fixed at 0 and never read.
        let (sa, sb, sc, sd) = unsafe {
            (
                if BC && !TREE { SLOT_INIT } else { *t.get_unchecked(ia) },
                *t.get_unchecked(ib),
                *t.get_unchecked(ic),
                if BC { SLOT_INIT } else { *t.get_unchecked(id) },
            )
        };
        // The flank stage uses the mixer input that the tree profile leaves idle (input 3): the adaptive probability of this decision
        // under the flank context. Its weight lives in the leaf's weight set like the other inputs.
        let fq = node * FL_CTX + self.fctx;
        let sf = if FL { self.fl.q[fq] } else { SLOT_INIT };
        let inp = [
            if BC && !TREE { 0 } else { s3_input(sa) },
            s3_input(sb),
            s3_input(sc),
            if FL { s3_input(sf) } else if BC { 0 } else { s3_input(sd) },
            256,
        ];
        let ws = unsafe { wt.get_unchecked_mut(wb + node) };
        // |in| <= 2047 and |w| <= 2^20, so each product fits i32 exactly; the sum is taken in i64 as before.
        let dot = (inp[0] * ws[0]) as i64 + (inp[1] * ws[1]) as i64 + (inp[2] * ws[2]) as i64 + (inp[3] * ws[3]) as i64 + (inp[4] * ws[4]) as i64;
        let x = (dot >> 16).clamp(-2047, 2047) as i32;
        let p12 = SQUASH[(x + 2048) as usize] as i32;
        let p16 = (p12 * 16 + 8).clamp(COD_P_MIN, COD_P_MAX);
        let bit = io.code(p16 as u32, want);
        let err = ((bit as i32) << 12) - p12;
        for i in 0..S3_IN {
            ws[i] = (ws[i] + ((inp[i] * err) >> shift)).clamp(-S3_W_CLAMP, S3_W_CLAMP);
        }
        if !BC || TREE {
            s3_update_slot(unsafe { t.get_unchecked_mut(ia) }, bit, limit);
        }
        s3_update_slot(unsafe { t.get_unchecked_mut(ib) }, bit, limit);
        s3_update_slot(unsafe { t.get_unchecked_mut(ic) }, bit, limit);
        if !BC {
            s3_update_slot(unsafe { t.get_unchecked_mut(id) }, bit, limit);
        }
        if FL {
            s3_update_slot(&mut self.fl.q[fq], bit, limit);
        }
        bit
    }

    /// Code one residual. Encoder passes the true value; decoder passes anything (ignored) and gets the decoded value.
    #[inline(always)]
    pub fn code_sample<I: BitIo>(&mut self, io: &mut I, r: i32) -> i32 {
        match self.profile {
            Stage3Profile::Legacy4 => self.code_sample_g::<I, false, false, false>(io, r, 0),
            Stage3Profile::Bc255S11 | Stage3Profile::Bc255S11Direct => self.code_sample_g::<I, true, false, false>(io, r, 0),
            Stage3Profile::Bc255S11Tree => panic!("Stage3Profile::Bc255S11Tree needs code_sample_leaf"),
        }
    }

    /// Like [`Stage3::code_sample`] for the `Bc255S11Tree` profile; `leaf < TREE_LEAVES` comes from [`tree_leaf`].
    #[inline(always)]
    pub fn code_sample_leaf<I: BitIo>(&mut self, io: &mut I, r: i32, leaf: usize) -> i32 {
        assert!(self.profile == Stage3Profile::Bc255S11Tree && leaf < TREE_LEAVES);
        if self.fl_on {
            self.code_sample_g::<I, true, true, true>(io, r, leaf)
        } else {
            self.code_sample_g::<I, true, true, false>(io, r, leaf)
        }
    }

    #[inline(always)]
    fn code_sample_g<I: BitIo, const BC: bool, const TREE: bool, const FL: bool>(&mut self, io: &mut I, r: i32, leaf: usize) -> i32 {
        let x = self.x;
        let i = x + 1;
        if x == 0 {
            // Left pad of a new row = magnitude of N (zero in row 0, where prev is all zero).
            self.cur_m[0] = self.prev_m[1];
            self.cur_s[0] = 1;
        }
        let (aw, an, anw, ane) = (self.cur_m[i - 1], self.prev_m[i], self.prev_m[i - 1], self.prev_m[i + 1]);
        let sum = 2 * aw + 2 * an + anw + ane;
        if FL {
            self.fctx = self.fbase + (((32 - (aw + an).leading_zeros()) as usize + 1) / 2).min(FL_QA - 1);
        }
        let qs = s3q(sum).min(NQ - 1);
        let qd = s3q(aw.abs_diff(an)).min(ND - 1);
        let qe = s3q((self.e >> 2) as u32).min(NQ - 1);
        let qw = s3q(aw).min(NW - 1);
        let a_ctx = if TREE { S3_SLOTS + (leaf * NQ + qs) * 64 } else { OFF_A + qs * 64 };
        let idx = [a_ctx, OFF_B + (qs * ND + qd) * 64, OFF_C + (qe * NW + qw) * 64];
        let wb = if TREE { leaf * 64 } else { (qs >> 3) * 64 };
        let sgi = self.cur_s[i - 1] as usize * 3 + self.prev_s[i] as usize;

        let a_in = r.unsigned_abs();
        let k_in = 32 - a_in.leading_zeros();
        let nz = self.code_bit::<I, BC, TREE, FL>(io, idx, wb, 0, (a_in != 0) as u32);
        let mut a = 0u32;
        let mut neg = 0u32;
        if nz != 0 {
            let mut k = 1u32;
            while k < self.kmax {
                if self.code_bit::<I, BC, TREE, FL>(io, idx, wb, k as usize, (k_in > k) as u32) == 0 {
                    break;
                }
                k += 1;
            }
            a = 1;
            if k >= 2 {
                let t = self.code_bit::<I, BC, TREE, FL>(io, idx, wb, 15 + k as usize, (a_in >> (k - 2)) & 1);
                a = 2 + t;
                if k >= 3 {
                    let u = self.code_bit::<I, BC, TREE, FL>(io, idx, wb, 33 + 2 * (k as usize - 3) + t as usize, (a_in >> (k - 3)) & 1);
                    a = 2 * a + u;
                    for b in (0..k - 3).rev() {
                        a = 2 * a + io.code(32768, (a_in >> b) & 1);
                    }
                }
            }
            let ps = (self.sg[sgi] >> 16).clamp(32, 65535 - 32);
            neg = if FL { self.code_sign_flank(io, ps, (r < 0) as u32) } else { io.code(ps, (r < 0) as u32) };
            s3_update_slot(&mut self.sg[sgi], neg, if BC { S3_LIMIT_BC } else { S3_LIMIT });
        }

        self.cur_m[i] = a;
        self.cur_s[i] = if a == 0 { 1 } else { 2 - 2 * neg as u8 };
        self.e += (((a as i32) << 4) - self.e) >> 4;
        self.x += 1;
        if self.x == self.w {
            self.x = 0;
            std::mem::swap(&mut self.cur_m, &mut self.prev_m);
            std::mem::swap(&mut self.cur_s, &mut self.prev_s);
            let w = self.w;
            self.prev_m[0] = self.prev_m[1];
            self.prev_m[w + 1] = self.prev_m[w];
            self.prev_s[0] = self.prev_s[1];
            self.prev_s[w + 1] = self.prev_s[w];
        }
        if neg != 0 { -(a as i32) } else { a as i32 }
    }
}

// ---------------------------------------------------------------------------------------------------
// Stage 4: entropy coding
//
// Binary range coder driven directly by the Stage 3 probability stream (`BitIo::code(p1, bit)`, p1 = P(bit = 1) on
// a 16-bit scale, 32..=65503; raw mantissa bits arrive as p1 = 32768).
//   * 32-bit range kept in [2^24, 2^32); split `bound = (range * p1) >> 16` with one 32x16 multiply in u64 (no division,
//     no precision loss from pre-shifting the range). bit 1 takes the lower part [0, bound).
//   * Carry: 64-bit `low` plus a (cache byte, run of 0xFF) pair, so output bytes are written once and never revisited.
//   * The always-zero leading byte of this carry scheme is never stored (the decoder starts by loading 4 bytes).
//   * Termination: the final interval [low, low + range) is narrowed to the value with the most trailing zero bits, the
//     low 5 bytes are flushed, then trailing zero bytes are dropped; the decoder reads implicit zeros past the end.
//     Cost is 0..=4 bytes. The stream carries no length or header; geometry and plane count belong to the container.
// ---------------------------------------------------------------------------------------------------

const S4_TOP: u32 = 1 << 24;

/// Stage 4 encoder. Reusable: [`Stage4Enc::reset`] keeps the output buffer's capacity.
pub struct Stage4Enc {
    low: u64,
    range: u32,
    cache: u8,
    ff: usize,
    lead: bool,
    out: Vec<u8>,
}

impl Stage4Enc {
    pub fn new() -> Self {
        Self::with_capacity(0)
    }

    pub fn with_capacity(cap: usize) -> Self {
        Self { low: 0, range: u32::MAX, cache: 0, ff: 0, lead: true, out: Vec::with_capacity(cap) }
    }

    pub fn reset(&mut self) {
        self.low = 0;
        self.range = u32::MAX;
        self.cache = 0;
        self.ff = 0;
        self.lead = true;
        self.out.clear();
    }

    /// Bytes already committed to the output (excludes the cached byte, pending 0xFF run and the final flush).
    pub fn bytes_so_far(&self) -> usize {
        self.out.len()
    }

    #[inline(always)]
    fn shift_low(&mut self) {
        let low = self.low;
        if (low as u32) < 0xFF00_0000 || (low >> 32) != 0 {
            let carry = (low >> 32) as u8;
            if self.lead {
                self.lead = false; // the first byte is always 0 (no carry can reach it)
            } else {
                self.out.push(self.cache.wrapping_add(carry));
            }
            if self.ff != 0 {
                let n = self.out.len() + self.ff;
                self.out.resize(n, 0xFFu8.wrapping_add(carry));
                self.ff = 0;
            }
            self.cache = (low >> 24) as u8;
        } else {
            self.ff += 1;
        }
        self.low = (low & 0x00FF_FFFF) << 8;
    }

    /// Terminate the stream and return its bytes. After this call only [`Stage4Enc::reset`] is meaningful.
    pub fn finish(&mut self) -> &[u8] {
        // Pick the value in [low, low + range) with the most trailing zero bits.
        let hi = self.low + self.range as u64; // exclusive
        for k in (0..=32).rev() {
            let m = (1u64 << k) - 1;
            let v = (self.low + m) & !m;
            if v < hi {
                self.low = v;
                break;
            }
        }
        for _ in 0..5 {
            self.shift_low();
        }
        while self.out.last() == Some(&0) {
            self.out.pop();
        }
        &self.out
    }
}

impl Default for Stage4Enc {
    fn default() -> Self {
        Self::new()
    }
}

impl BitIo for Stage4Enc {
    #[inline(always)]
    fn code(&mut self, p1: u32, bit: u32) -> u32 {
        let bound = ((self.range as u64 * p1 as u64) >> 16) as u32;
        if bit != 0 {
            self.range = bound;
        } else {
            self.low += bound as u64;
            self.range -= bound;
        }
        while self.range < S4_TOP {
            self.range <<= 8;
            self.shift_low();
        }
        bit
    }
}

/// Stage 4 decoder over a borrowed buffer; bytes past the end read as zero (see termination above).
pub struct Stage4Dec<'a> {
    code: u32,
    range: u32,
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Stage4Dec<'a> {
    pub fn new(buf: &'a [u8]) -> Self {
        let mut code = 0u32;
        for i in 0..4 {
            code = (code << 8) | buf.get(i).copied().unwrap_or(0) as u32;
        }
        Self { code, range: u32::MAX, buf, pos: 4 }
    }

    /// Bytes consumed so far (counts the implicit zero bytes past the end of `buf`).
    pub fn consumed(&self) -> usize {
        self.pos
    }
}

impl BitIo for Stage4Dec<'_> {
    #[inline(always)]
    fn code(&mut self, p1: u32, _bit: u32) -> u32 {
        let bound = ((self.range as u64 * p1 as u64) >> 16) as u32;
        let bit = if self.code < bound {
            self.range = bound;
            1
        } else {
            self.code -= bound;
            self.range -= bound;
            0
        };
        while self.range < S4_TOP {
            self.code = (self.code << 8) | self.buf.get(self.pos).copied().unwrap_or(0) as u32;
            self.pos += 1;
            self.range <<= 8;
        }
        bit
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EllieError {
    Stage1(Stage1Error),
    Stage3(Stage3Error),
    /// Plane length does not match the codec geometry.
    BadPlanes,
    /// A flank topology that reads a plane which is not coded before the plane it conditions, or an empty plane.
    BadTopology,
}

impl From<Stage1Error> for EllieError {
    fn from(e: Stage1Error) -> Self {
        Self::Stage1(e)
    }
}

impl From<Stage3Error> for EllieError {
    fn from(e: Stage3Error) -> Self {
        Self::Stage3(e)
    }
}

/// Stage 1 -> probability model -> range coder for a set of equally sized planes sharing one coder stream. Allocates once;
/// planes are coded in order. The probability model (`Stage3`) restarts per plane unless `carry_model` is set, in which case its
/// adaptive state (and neighbour rows) simply continue into the next plane; encoder and decoder do the same either way.
pub struct EllieCodec {
    s1: Stage1,
    s3: Stage3,
    res: Vec<i32>,
    leaf: Vec<u8>,
    n: usize,
    /// Legacy four-stage profiles apply [`Stage2`] bias correction per plane; `Bc255S11Direct` does not.
    bias: bool,
    tree: bool,
    pub carry_model: bool,
}

impl EllieCodec {
    /// Codec with the [`Stage3Profile::Legacy4`] Stage 3 profile.
    pub fn new(width: usize, height: usize, bits: u32) -> Result<Self, EllieError> {
        Self::with_profile(width, height, bits, Stage3Profile::Legacy4)
    }

    pub fn with_profile(width: usize, height: usize, bits: u32, profile: Stage3Profile) -> Result<Self, EllieError> {
        Ok(Self {
            s1: Stage1::new(width, height, bits)?,
            s3: Stage3::with_profile(width, height, bits, profile)?,
            res: vec![0; width * height],
            leaf: if profile == Stage3Profile::Bc255S11Tree { vec![0; width * height] } else { Vec::new() },
            n: width * height,
            bias: matches!(profile, Stage3Profile::Legacy4 | Stage3Profile::Bc255S11),
            tree: profile == Stage3Profile::Bc255S11Tree,
            carry_model: false,
        })
    }

    /// Encode all planes through `enc` (normally a [`Stage4Enc`], whose caller then calls [`Stage4Enc::finish`]).
    pub fn encode_planes<I: BitIo>(&mut self, planes: &[&[u16]], enc: &mut I) -> Result<(), EllieError> {
        for (i, p) in planes.iter().enumerate() {
            if p.len() != self.n {
                return Err(EllieError::BadPlanes);
            }
            if i == 0 || !self.carry_model {
                self.s3.reset();
            }
            if self.tree {
                self.s1.encode_tree(p, &mut self.res, &mut self.leaf)?;
                self.s3.encode_plane_leaf(&self.res, &self.leaf, enc)?;
                continue;
            }
            self.s1.encode(p, &mut self.res)?;
            if self.bias {
                Stage2::new().encode_in_place(&mut self.res);
            }
            self.s3.encode_plane(&self.res, enc)?;
        }
        Ok(())
    }

    /// Decode `planes.len()` planes from `bytes` (a finished Stage 4 stream) into the given buffers.
    pub fn decode_planes(&mut self, bytes: &[u8], planes: &mut [&mut [u16]]) -> Result<(), EllieError> {
        let mut dec = Stage4Dec::new(bytes);
        for (i, p) in planes.iter_mut().enumerate() {
            if p.len() != self.n {
                return Err(EllieError::BadPlanes);
            }
            if i == 0 || !self.carry_model {
                self.s3.reset();
            }
            if self.tree {
                self.s1.decode_tree(&mut self.s3, &mut dec, p)?;
                continue;
            }
            self.s3.decode_plane(&mut self.res, &mut dec)?;
            if self.bias {
                Stage2::new().decode_in_place(&mut self.res);
            }
            self.s1.decode(&self.res, p)?;
        }
        Ok(())
    }
}

// ---------------------------------------------------------------- cross-plane flank codec (ULISA)

/// One link of a flank topology: the residuals at `a` and `b` (offsets `(dx, dy)` from the conditioned site's plane coordinates,
/// clamped to the source plane) of an earlier-coded plane are summed into the flank sum.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FlankLink {
    pub plane: u8,
    pub a: (i8, i8),
    pub b: (i8, i8),
}

/// Which already-coded planes condition each of the four planes. Supplied by the sensor frame (the Bayer geometry is not
/// ELLIE's business); validated so a link can only read a plane that is coded earlier.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FlankTopology {
    links: [Vec<FlankLink>; 4],
}

impl FlankTopology {
    pub fn new(links: [Vec<FlankLink>; 4]) -> Result<Self, EllieError> {
        for (k, ls) in links.iter().enumerate() {
            if ls.iter().any(|l| (l.plane as usize) >= k) {
                return Err(EllieError::BadTopology);
            }
        }
        Ok(Self { links })
    }
    pub fn links(&self, plane: usize) -> &[FlankLink] {
        &self.links[plane]
    }
}

/// How often the flank context was informative, counted while coding.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FlankStats {
    pub samples: u64,
    /// Samples whose flank sum was non-zero (the context differed from the neutral one).
    pub nonzero: u64,
}

/// Four planes (parity order) in one range-coder stream. Per plane: Stage 1 -> `Bc255S11Tree` model with the flank stage -> coder.
/// The tree model restarts per plane; the flank state is shared by the planes of the image. Planes may differ in size.
pub struct FlankCodec {
    dims: [(usize, usize); 4],
    bits: u32,
    topo: FlankTopology,
    pub stats: FlankStats,
}

/// Flank context bases of every site of plane `k`, from the stored residuals of earlier planes (one pass over the plane; the encoder
/// and decoder both run it before the plane is coded, which is possible because the flank only reads planes coded earlier).
/// Columns whose flank offsets stay inside the source plane (the vast majority) are summed as plain slices; the rest clamp each
/// coordinate to the source plane.
fn flank_bases(topo: &FlankTopology, dims: &[(usize, usize); 4], stored: &[Vec<i16>; 4], k: usize, stats: &mut FlankStats) -> Vec<u8> {
    let (w, h) = dims[k];
    let mut sums = vec![0i32; w * h];
    for l in topo.links(k) {
        let (sw, sh) = (dims[l.plane as usize].0 as isize, dims[l.plane as usize].1 as isize);
        let src = &stored[l.plane as usize];
        let (ax, ay, bx, by) = (l.a.0 as isize, l.a.1 as isize, l.b.0 as isize, l.b.1 as isize);
        let at = |x: isize, y: isize| src[(y.clamp(0, sh - 1) * sw + x.clamp(0, sw - 1)) as usize] as i32;
        // Columns j with j + ax and j + bx both inside [0, sw).
        let jlo = 0.max(-ax).max(-bx).min(w as isize);
        let jhi = (w as isize).min(sw - ax).min(sw - bx).max(jlo);
        for y in 0..h as isize {
            let row = &mut sums[y as usize * w..(y as usize + 1) * w];
            let rows_inside = (0..sh).contains(&(y + ay)) && (0..sh).contains(&(y + by));
            if rows_inside && jlo < jhi {
                let (ra, rb) = ((y + ay) * sw, (y + by) * sw);
                let sa = &src[(ra + jlo + ax) as usize..(ra + jhi + ax) as usize];
                let sb = &src[(rb + jlo + bx) as usize..(rb + jhi + bx) as usize];
                for ((o, &va), &vb) in row[jlo as usize..jhi as usize].iter_mut().zip(sa).zip(sb) {
                    *o += va as i32 + vb as i32;
                }
                for j in (0..jlo).chain(jhi..w as isize) {
                    row[j as usize] += at(j + ax, y + ay) + at(j + bx, y + by);
                }
            } else {
                for j in 0..w as isize {
                    row[j as usize] += at(j + ax, y + ay) + at(j + bx, y + by);
                }
            }
        }
    }
    stats.samples += sums.len() as u64;
    let mut nonzero = 0u64;
    let bases = sums
        .iter()
        .map(|&v| {
            nonzero += (v != 0) as u64;
            flank_base(v) as u8
        })
        .collect();
    stats.nonzero += nonzero;
    bases
}

impl FlankCodec {
    pub fn new(dims: [(usize, usize); 4], bits: u32, topo: FlankTopology) -> Result<Self, EllieError> {
        if dims.iter().any(|&(w, h)| w == 0 || h == 0) {
            return Err(EllieError::BadTopology);
        }
        Stage1::new(dims[0].0, dims[0].1, bits)?;
        Ok(Self { dims, bits, topo, stats: FlankStats::default() })
    }

    pub fn encode_planes<I: BitIo>(&mut self, planes: [&[u16]; 4], enc: &mut I) -> Result<(), EllieError> {
        let mut stored: [Vec<i16>; 4] = Default::default();
        let mut fm = FlankModel::new();
        for k in 0..4 {
            let (w, h) = self.dims[k];
            if planes[k].len() != w * h {
                return Err(EllieError::BadPlanes);
            }
            let mut s1 = Stage1::new(w, h, self.bits)?;
            let (mut res, mut leaf) = (vec![0i32; w * h], vec![0u8; w * h]);
            s1.encode_tree(planes[k], &mut res, &mut leaf)?;
            let mut s3 = Stage3::with_profile(w, h, self.bits, Stage3Profile::Bc255S11Tree)?;
            s3.attach_flank(fm);
            let bases = flank_bases(&self.topo, &self.dims, &stored, k, &mut self.stats);
            s3.encode_plane_leaf_flank(&res, &leaf, enc, &bases)?;
            fm = s3.detach_flank();
            stored[k] = res.iter().map(|&r| r.clamp(i16::MIN as i32, i16::MAX as i32) as i16).collect();
        }
        Ok(())
    }

    pub fn decode_planes(&mut self, bytes: &[u8], out: [&mut [u16]; 4]) -> Result<(), EllieError> {
        let mut dec = Stage4Dec::new(bytes);
        let mut stored: [Vec<i16>; 4] = Default::default();
        let mut fm = FlankModel::new();
        for (k, o) in out.into_iter().enumerate() {
            let (w, h) = self.dims[k];
            if o.len() != w * h {
                return Err(EllieError::BadPlanes);
            }
            let mut s1 = Stage1::new(w, h, self.bits)?;
            let mut res = vec![0i32; w * h];
            let mut s3 = Stage3::with_profile(w, h, self.bits, Stage3Profile::Bc255S11Tree)?;
            s3.attach_flank(fm);
            let bases = flank_bases(&self.topo, &self.dims, &stored, k, &mut self.stats);
            s1.decode_tree_flank(&mut s3, &mut dec, o, &mut res, &bases)?;
            fm = s3.detach_flank();
            stored[k] = res.iter().map(|&r| r.clamp(i16::MIN as i32, i16::MAX as i32) as i16).collect();
        }
        Ok(())
    }
}

// ---------------------------------------------------------------- colour transform (before Stage 1)

/// Offset that stores the signed chroma planes Co, Cg (each in `-255..=255` for 8-bit RGB) in the unsigned 9-bit domain.
pub const YCOCG_OFFSET: i32 = 255;
/// Bit depth of the Co and Cg planes for 8-bit RGB input (Y stays at 8 bits).
pub const YCOCG_CHROMA_BITS: u32 = 9;

/// Reversible YCoCg-R, forward. Signed integer intermediates and arithmetic shifts; exact for 8-bit R, G, B.
#[inline(always)]
pub fn ycocg_r_forward(r: i32, g: i32, b: i32) -> (i32, i32, i32) {
    let co = r - b;
    let t = b + (co >> 1);
    let cg = g - t;
    let y = t + (cg >> 1);
    (y, co, cg)
}

/// Exact inverse of [`ycocg_r_forward`].
#[inline(always)]
pub fn ycocg_r_inverse(y: i32, co: i32, cg: i32) -> (i32, i32, i32) {
    let t = y - (cg >> 1);
    let g = cg + t;
    let b = t - (co >> 1);
    (co + b, g, b)
}

/// In place on three equally sized 8-bit planes `[R, G, B]` -> `[Y, Co + 255, Cg + 255]`. Returns `false` (and leaves the
/// planes unspecified) if a sample exceeds 255.
pub fn ycocg_r_forward_planes(p: &mut [Vec<u16>]) -> bool {
    let [r, g, b] = p else { return false };
    if r.len() != g.len() || r.len() != b.len() {
        return false;
    }
    let mut over = 0u16;
    for i in 0..r.len() {
        over |= r[i] | g[i] | b[i];
        let (y, co, cg) = ycocg_r_forward(r[i] as i32, g[i] as i32, b[i] as i32);
        (r[i], g[i], b[i]) = (y as u16, (co + YCOCG_OFFSET) as u16, (cg + YCOCG_OFFSET) as u16);
    }
    over <= 255
}

/// In place `[Y, Co + 255, Cg + 255]` -> `[R, G, B]`. Returns `false` if any reconstructed sample leaves `0..=255` or an
/// input is outside its domain, which only a corrupt stream can cause.
pub fn ycocg_r_inverse_planes(p: &mut [Vec<u16>]) -> bool {
    let [y, co, cg] = p else { return false };
    if y.len() != co.len() || y.len() != cg.len() {
        return false;
    }
    let mut bad = 0u32;
    for i in 0..y.len() {
        bad |= (y[i] as u32 > 255) as u32 | (co[i] as u32 > 2 * YCOCG_OFFSET as u32) as u32 | (cg[i] as u32 > 2 * YCOCG_OFFSET as u32) as u32;
        let (r, g, b) = ycocg_r_inverse(y[i] as i32, co[i] as i32 - YCOCG_OFFSET, cg[i] as i32 - YCOCG_OFFSET);
        bad |= ((r | g | b) as u32 > 255) as u32;
        (y[i], co[i], cg[i]) = (r as u16, g as u16, b as u16);
    }
    bad == 0
}

#[cfg(test)]
mod ycocg_tests {
    use super::*;

    /// Every 8-bit RGB triple round-trips, and the transformed values stay inside the documented domains (no clamping).
    #[test]
    fn ycocg_r_is_exact_over_all_2_pow_24_triples() {
        for r in 0..256 {
            for g in 0..256 {
                for b in 0..256 {
                    let (y, co, cg) = ycocg_r_forward(r, g, b);
                    assert!((0..=255).contains(&y) && (-255..=255).contains(&co) && (-255..=255).contains(&cg));
                    assert_eq!(ycocg_r_inverse(y, co, cg), (r, g, b));
                }
            }
        }
    }

    #[test]
    fn plane_transform_round_trips_and_rejects_bad_input() {
        let n = 256 * 256;
        let mut p = vec![(0..n).map(|i| (i >> 8) as u16).collect::<Vec<_>>(), (0..n).map(|i| (i & 255) as u16).collect(), (0..n).map(|i| (i * 7 & 255) as u16).collect()];
        let orig = p.clone();
        assert!(ycocg_r_forward_planes(&mut p));
        assert!(p[1].iter().chain(&p[2]).all(|&v| v <= 510));
        assert!(ycocg_r_inverse_planes(&mut p));
        assert_eq!(p, orig);
        let mut big = vec![vec![256u16], vec![0], vec![0]];
        assert!(!ycocg_r_forward_planes(&mut big));
        let mut corrupt = vec![vec![0u16], vec![511], vec![0]];
        assert!(!ycocg_r_inverse_planes(&mut corrupt));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    struct Rng(u64);
    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }
    }

    fn pattern(kind: usize, w: usize, h: usize, bits: u32, rng: &mut Rng) -> Vec<u16> {
        let m = (1u32 << bits) - 1;
        let mut v = vec![0u16; w * h];
        for y in 0..h {
            for x in 0..w {
                let val = match kind {
                    0 => (rng.next() % (m as u64 + 1)) as u32,
                    1 => {
                        let base = ((x * 3 + y * 5) as u32 * (m / 64 + 1)) % (m + 1);
                        (base + (rng.next() % 5) as u32).min(m)
                    }
                    2 => 0,
                    3 => m,
                    4 => if (x + y) % 2 == 0 { 0 } else { m },
                    _ => ((x + y) as u32).min(m),
                };
                v[y * w + x] = val as u16;
            }
        }
        v
    }

    const SIZES: [(usize, usize); 10] = [(1, 1), (1, 7), (7, 1), (2, 2), (3, 3), (2, 9), (9, 2), (17, 13), (31, 5), (64, 48)];

    /// Naive reference: computes every neighbour and every difference directly from pixels.
    fn ref_encode(src: &[u16], w: usize, h: usize, bits: u32) -> Vec<i32> {
        let m = ((1u32 << bits) - 1) as i32;
        let mut res = vec![0i32; w * h];
        for x in 0..w {
            let p = if x == 0 {
                1i32 << (bits - 1)
            } else {
                let wv = src[x - 1] as i32;
                let wwv = if x >= 2 { src[x - 2] as i32 } else { wv };
                ((5 * wv - wwv + 2) >> 2).clamp(0, m)
            };
            res[x] = src[x] as i32 - p;
        }
        let mut shrub = Shrub { e: [0; 3] };
        for y in 1..h {
            for x in 0..w {
                let px = |dx: i64, dy: i64| -> i32 {
                    let (tx, ty) = (x as i64 + dx, (y as i64 + dy).max(0));
                    let (tx, ty) = (tx, ty as usize);
                    (if tx < 0 {
                        if ty == y {
                            if x == 0 { src[(y - 1) * w] } else { src[y * w] }
                        } else {
                            src[ty * w]
                        }
                    } else if tx as usize >= w {
                        src[ty * w + w - 1]
                    } else {
                        src[ty * w + tx as usize]
                    }) as i32
                };
                let (wv, wwv, n, nn) = (px(-1, 0), px(-2, 0), px(0, -1), px(0, -2));
                let (nw, ne, nnw, nne) = (px(-1, -1), px(1, -1), px(-1, -2), px(1, -2));
                let ad = |a: i32, b: i32| (a - b).unsigned_abs();
                let s_h = ad(wv, wwv) + ad(nw, n) + ad(n, ne);
                let s_v = ad(wv, nw) + ad(n, nn) + ad(ne, nne);
                let s45 = ad(wv, n) + ad(nw, nn);
                let s135 = ad(ne, nn) + ad(n, nnw);
                let (p, est) = predict(m, wv, wwv, n, nn, nw, ne, s_h, s_v, s45, s135, shrub.factors());
                shrub.update(src[y * w + x] as i32, est, m);
                res[y * w + x] = src[y * w + x] as i32 - p;
            }
        }
        res
    }

    #[test]
    fn roundtrip_all_bit_depths_and_edges() {
        let mut rng = Rng(0x9E3779B97F4A7C15);
        for bits in 1..=MAX_BITS {
            let m = (1u32 << bits) - 1;
            for &(w, h) in &SIZES {
                for kind in 0..6 {
                    let src = pattern(kind, w, h, bits, &mut rng);
                    let mut s = Stage1::new(w, h, bits).unwrap();
                    let mut res = vec![0i32; w * h];
                    let mut pred = vec![0u16; w * h];
                    s.encode_with_pred(&src, &mut res, &mut pred).unwrap();
                    for i in 0..w * h {
                        assert!(pred[i] as u32 <= m);
                        assert_eq!(res[i], src[i] as i32 - pred[i] as i32);
                        assert!(fold(res[i], pred[i] as u32, m) <= m);
                    }
                    let mut out = vec![0u16; w * h];
                    s.decode(&res, &mut out).unwrap();
                    assert_eq!(out, src, "bits={bits} {w}x{h} kind={kind}");
                    // Optimized scan (shared difference planes) equals the naive reference.
                    assert_eq!(res, ref_encode(&src, w, h, bits), "ref mismatch bits={bits} {w}x{h} kind={kind}");
                }
            }
        }
    }

    #[test]
    fn rejects_bad_input() {
        assert_eq!(Stage1::new(4, 4, 0).err(), Some(Stage1Error::BadBits(0)));
        assert_eq!(Stage1::new(4, 4, 17).err(), Some(Stage1Error::BadBits(17)));
        let mut s = Stage1::new(2, 2, 8).unwrap();
        let mut res = [0i32; 4];
        assert_eq!(s.encode(&[0, 0, 0, 256], &mut res), Err(Stage1Error::SampleOutOfRange));
        assert_eq!(s.encode(&[0, 0, 0], &mut res), Err(Stage1Error::BadLength));
        let mut out = [0u16; 4];
        assert_eq!(s.decode(&[1000, 0, 0, 0], &mut out), Err(Stage1Error::ResidualOutOfRange));
        let mut e = Stage1::new(0, 5, 8).unwrap();
        e.encode(&[], &mut []).unwrap();
    }

    #[test]
    fn fold_unfold_bijection() {
        for bits in 1..=10u32 {
            let m = (1u32 << bits) - 1;
            for p in 0..=m {
                let mut seen = vec![false; m as usize + 1];
                for x in 0..=m {
                    let e = x as i32 - p as i32;
                    let u = fold(e, p, m);
                    assert!(u <= m);
                    assert!(!seen[u as usize], "collision bits={bits} p={p}");
                    seen[u as usize] = true;
                    assert_eq!(unfold(u, p, m), e);
                }
                assert!(seen.iter().all(|&b| b));
            }
        }
        let m = 65535u32;
        let mut ps: Vec<u32> = (0..=m).step_by(257).collect();
        ps.extend([0, 1, 2, 3, 32767, 32768, 65532, 65533, 65534, 65535]);
        for &p in &ps {
            let mut seen = vec![false; m as usize + 1];
            for x in 0..=m {
                let e = x as i32 - p as i32;
                let u = fold(e, p, m);
                assert!(u <= m && !seen[u as usize]);
                seen[u as usize] = true;
                assert_eq!(unfold(u, p, m), e);
            }
        }
    }

    /// Exact big-integer reference of the mixing formula; also asserts the u64 bounds from the module docs.
    #[test]
    fn predict_matches_u128_reference_and_bounds() {
        fn reference(m: i32, nb: [i32; 6], s: [u32; 4]) -> i32 {
            let [w, ww, n, nn, nw, ne] = nb;
            let [s_h, s_v, s45, s135] = s;
            let mn = s45.min(s135);
            let s_d = mn + (mn >> 2);
            let smax = s_h.max(s_v).max(s_d);
            let mut sh = 0u32;
            while (smax >> sh) >= 1024 {
                sh += 1;
            }
            let q = |x: u32| (((x >> sh) + 1) as u128).pow(2);
            let (qh, qv, qd) = (q(s_h), q(s_v), q(s_d));
            let f: i128 = if s135 >= 2 * s45 && s45 >= 2 * s135 { 2 } else if s135 >= 2 * s45 { 3 } else if s45 >= 2 * s135 { 1 } else { 2 };
            let o = 4 * m as i128;
            let h4 = (4 * w + ((w - ww + n - nw) >> 1)) as i128 + o;
            let v4 = (4 * n + ((n - nn + w - nw) >> 1)) as i128 + o;
            let d4 = (3 * ((4 - f) * nw as i128 + f * ne as i128)) / 4 + (w + ne - n) as i128 + o;
            let pp = (w + n - nw) as i128;
            let pe = [w, n, nw].into_iter().min_by_key(|&c| ((pp - c as i128).abs(), [w, n, nw].iter().position(|&x| x == c).unwrap())).unwrap();
            let d4 = (d4 + 4 * pe as i128 + o + 1) >> 1;
            assert!(h4 >= 0 && v4 >= 0 && d4 >= 0 && h4.max(v4).max(d4) < 9 * (m as i128 + 1));
            let (ph, pv, pd) = (qv * qd, qh * qd, qh * qv);
            let d = ph + pv + pd;
            let num = ph * h4 as u128 + pv * v4 as u128 + pd * d4 as u128;
            assert!(d < 1u128 << 42, "D bound");
            assert!(num < 1u128 << 62, "N' bound");
            // Round-half-up of (num/d - 4M)/4 computed from the exact rational.
            let t = num / d;
            ((((t + 2) >> 2) as i128 - m as i128).clamp(0, m as i128)) as i32
        }
        let m = 65535i32;
        let svals = [0u32, 1, 2, 1023, 1024, 1025, 4095, 65535, 131071, 3 * 65535];
        let nbs: Vec<[i32; 6]> = (0..64).map(|k| { let mut a = [0; 6]; for j in 0..6 { a[j] = if k >> j & 1 == 1 { m } else { 0 }; } a }).collect();
        for &a in &svals { for &b in &svals { for &c in &svals { for &d in &svals {
            for nb in &nbs {
                let got = predict(m, nb[0], nb[1], nb[2], nb[3], nb[4], nb[5], a, b, c, d, [256; 3]).0;
                assert_eq!(got, reference(m, *nb, [a, b, c, d]));
                assert!((0..=m).contains(&got));
            }
        }}}}
        let mut rng = Rng(12345);
        for bits in 1..=16u32 {
            let m = (1i32 << bits) - 1;
            let mx = 3 * m as u64 + 1;
            for _ in 0..20000 {
                let mut nb = [0i32; 6];
                for v in nb.iter_mut() { *v = (rng.next() % (m as u64 + 1)) as i32; }
                let sv = |r: &mut Rng| (r.next() % mx) as u32;
                let s = [sv(&mut rng), sv(&mut rng), sv(&mut rng), sv(&mut rng)];
                let got = predict(m, nb[0], nb[1], nb[2], nb[3], nb[4], nb[5], s[0], s[1], s[2], s[3], [256; 3]).0;
                assert_eq!(got, reference(m, nb, s));
            }
        }
    }

    /// Rounding sanity: a flat region predicts itself exactly, and a pure ramp rounds half up.
    #[test]
    fn flat_and_rounding() {
        for bits in [1u32, 8, 12, 16] {
            let m = (1i32 << bits) - 1;
            for v in [0, 1, m / 2, m - 1, m] {
                assert_eq!(predict(m, v, v, v, v, v, v, 0, 0, 0, 0, [256; 3]).0, v);
            }
        }
        // Equal weights (all S equal), predictors 3/4-valued: P4 mean = 4*value => exact.
        assert_eq!(predict(255, 10, 10, 10, 10, 10, 10, 5, 5, 5, 5, [256; 3]).0, 10);
    }

    fn fnv(res: &[i32]) -> u64 {
        let mut h = 0xcbf29ce484222325u64;
        for &r in res {
            for b in r.to_le_bytes() {
                h = (h ^ b as u64).wrapping_mul(0x100000001b3);
            }
        }
        h
    }

    #[test]
    fn deterministic_across_runs_and_instances() {
        let mut rng = Rng(777);
        let (w, h) = (97, 61);
        let src = pattern(1, w, h, 12, &mut rng);
        let mut s = Stage1::new(w, h, 12).unwrap();
        let mut a = vec![0i32; w * h];
        let mut b = vec![0i32; w * h];
        s.encode(&src, &mut a).unwrap();
        s.encode(&src, &mut b).unwrap(); // buffer reuse
        let mut c = vec![0i32; w * h];
        Stage1::new(w, h, 12).unwrap().encode(&src, &mut c).unwrap();
        assert_eq!(a, b);
        assert_eq!(a, c);
        // Regression pin (generated by this implementation, not an independent oracle).
        assert_eq!(fnv(&a), 308058875918215606, "regression pin");
    }

    // ---------- Measurements (run with: cargo test --release --lib ellie -- --ignored --nocapture) ----------

    fn med_res(src: &[u16], w: usize, h: usize, m: u32) -> Vec<(i32, u32)> {
        let px = |x: i64, y: i64| -> i32 {
            let y = y.max(0) as usize;
            let x = x.clamp(0, w as i64 - 1) as usize;
            src[y * w + x] as i32
        };
        let mut o = Vec::with_capacity(w * h);
        for y in 0..h as i64 {
            for x in 0..w as i64 {
                let p = if y == 0 && x == 0 { (m as i32 + 1) / 2 } else if y == 0 { px(x - 1, 0) } else if x == 0 { px(0, y - 1) } else {
                    let (a, b, c) = (px(x - 1, y), px(x, y - 1), px(x - 1, y - 1));
                    if c >= a.max(b) { a.min(b) } else if c <= a.min(b) { a.max(b) } else { a + b - c }
                };
                let p = p.clamp(0, m as i32) as u32;
                o.push((src[y as usize * w + x as usize] as i32 - p as i32, p));
            }
        }
        o
    }

    fn entropy(u: impl Iterator<Item = u32>, m: u32) -> f64 {
        let mut hist = vec![0u64; m as usize + 1];
        let mut n = 0u64;
        for v in u { hist[v as usize] += 1; n += 1; }
        hist.iter().filter(|&&c| c > 0).map(|&c| { let p = c as f64 / n as f64; -p * p.log2() }).sum()
    }

    #[test]
    #[ignore]
    fn measure_real_images_entropy() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("historical/20261027-diag1-residual-maps/maps");
        let Ok(rd) = std::fs::read_dir(&root) else { println!("no images at {root:?}"); return };
        let (mut te, mut tm, mut tn) = (0.0, 0.0, 0.0);
        for ent in rd.flatten() {
            let f = ent.path().join("source.png");
            let Ok(img) = image::open(&f) else { continue };
            let g = img.to_luma8();
            let (w, h) = (g.width() as usize, g.height() as usize);
            let src: Vec<u16> = g.as_raw().iter().map(|&v| v as u16).collect();
            let mut s = Stage1::new(w, h, 8).unwrap();
            let (mut res, mut pred) = (vec![0i32; w * h], vec![0u16; w * h]);
            s.encode_with_pred(&src, &mut res, &mut pred).unwrap();
            let he = entropy(res.iter().zip(&pred).map(|(&e, &p)| fold(e, p as u32, 255)), 255);
            let med = med_res(&src, w, h, 255);
            let hm = entropy(med.iter().map(|&(e, p)| fold(e, p, 255)), 255);
            println!("{:>14} {w}x{h}  ELLIE S1 {he:.3} bpp   MED {hm:.3} bpp   ({:+.2}%)", ent.file_name().to_string_lossy(), (he / hm - 1.0) * 100.0);
            te += he; tm += hm; tn += 1.0;
        }
        if tn > 0.0 { println!("mean: ELLIE S1 {:.3} bpp, MED {:.3} bpp (order-0 entropy of folded residuals, 8-bit luma)", te / tn, tm / tn); }
    }

    #[test]
    #[ignore]
    fn measure_throughput() {
        let (w, h) = (2048usize, 2048usize);
        let mut rng = Rng(42);
        for bits in [8u32, 12, 16] {
            let m = (1u32 << bits) - 1;
            let mut src = vec![0u16; w * h];
            for y in 0..h {
                for x in 0..w {
                    let s = ((x as f64 * 0.01).sin() + (y as f64 * 0.013).cos()) * 0.25 + 0.5;
                    let n = (rng.next() % 17) as f64 / 17.0 - 0.5;
                    src[y * w + x] = (((s + n * 0.02).clamp(0.0, 1.0)) * m as f64) as u16;
                }
            }
            let mut s = Stage1::new(w, h, bits).unwrap();
            let mut res = vec![0i32; w * h];
            let mut out = vec![0u16; w * h];
            let (mut te, mut td) = (f64::MAX, f64::MAX);
            for _ in 0..7 {
                let t = Instant::now();
                s.encode(&src, &mut res).unwrap();
                te = te.min(t.elapsed().as_secs_f64());
                let t = Instant::now();
                s.decode(&res, &mut out).unwrap();
                td = td.min(t.elapsed().as_secs_f64());
            }
            assert_eq!(out, src);
            let n = (w * h) as f64;
            println!("{bits:>2}-bit {w}x{h}: encode {:.2} ns/px ({:.1} Mpx/s)  decode {:.2} ns/px ({:.1} Mpx/s)", te / n * 1e9, n / te / 1e6, td / n * 1e9, n / td / 1e6);
            if bits == 8 {
                let t = Instant::now();
                let mut acc = 0i64;
                for _ in 0..7 { acc += med_res(&src, w, h, m).iter().map(|&(e, _)| e as i64).sum::<i64>(); }
                println!("   (reference MED loop incl. its allocation: {:.2} ns/px, sink {acc})", t.elapsed().as_secs_f64() / 7.0 / n * 1e9);
            }
        }
    }

    // ---- Stage 2 ----

    fn entropy_i32(v: &[i32]) -> f64 {
        let mut h = std::collections::HashMap::new();
        for &x in v { *h.entry(x).or_insert(0usize) += 1; }
        let n = v.len() as f64;
        -h.values().map(|&c| { let p = c as f64 / n; p * p.log2() }).sum::<f64>()
    }

    /// Q: does r invert exactly, and do encoder/decoder states match at every step, for all bit depths,
    /// including worst-case +/-M alternation and saturated runs (overflow check)?
    #[test]
    fn stage2_inversion_and_state_lockstep() {
        let mut rng = Rng(0x9e3779b97f4a7c15);
        for bits in 1..=16u32 {
            let m = (1i32 << bits) - 1;
            for kind in 0..5 {
                let e: Vec<i32> = (0..20000).map(|i| match kind {
                    0 => (rng.next() % (2 * m as u64 + 1)) as i32 - m,
                    1 => if i % 2 == 0 { m } else { -m },
                    2 => m,
                    3 => -m,
                    _ => if (i / 700) % 2 == 0 { m } else { -m },
                }).collect();
                let (mut a, mut b) = (Stage2::new(), Stage2::new());
                assert_eq!(a.state(), 0);
                for &x in &e {
                    let r = a.encode_one(x);
                    assert!(r.abs() <= 2 * m && a.bias().abs() <= m);
                    assert_eq!(b.decode_one(r), x);
                    assert_eq!(a, b);
                }
                let mut buf = e.clone();
                let mut c = Stage2::new();
                c.encode_in_place(&mut buf);
                let mut d = Stage2::new();
                let mut buf2 = buf.clone();
                d.decode_in_place(&mut buf2);
                assert_eq!(buf2, e);
                assert_eq!(c, d);
                assert_eq!(c, a);
            }
        }
    }

    /// Q: does Stage 1 + Stage 2 reconstruct exactly, deterministically, on all patterns/sizes/depths?
    #[test]
    fn stage2_composed_with_stage1() {
        let mut rng = Rng(12345);
        for bits in [1u32, 2, 8, 12, 16] {
            for &(w, h) in &SIZES {
                for kind in 0..6 {
                    let src = pattern(kind, w, h, bits, &mut rng);
                    let mut s1 = Stage1::new(w, h, bits).unwrap();
                    let mut res = vec![0i32; w * h];
                    s1.encode(&src, &mut res).unwrap();
                    let e0 = res.clone();
                    Stage2::new().encode_in_place(&mut res);
                    let first = res.clone();
                    let mut again = e0.clone();
                    Stage2::new().encode_in_place(&mut again);
                    assert_eq!(first, again);
                    Stage2::new().decode_in_place(&mut res);
                    assert_eq!(res, e0);
                    let mut out = vec![0u16; w * h];
                    s1.decode(&res, &mut out).unwrap();
                    assert_eq!(out, src);
                }
            }
        }
    }

    /// Q: no state divergence over a long sequence (10M samples) with a drifting bias?
    #[test]
    fn stage2_long_sequence_no_divergence() {
        let mut rng = Rng(777);
        let n = 10_000_000;
        let e: Vec<i32> = (0..n).map(|i| ((rng.next() % 41) as i32 - 20) + (((i / 100_000) % 7) as i32 - 3) * 9).collect();
        let mut buf = e.clone();
        let mut a = Stage2::new();
        a.encode_in_place(&mut buf);
        let mut b = Stage2::new();
        b.decode_in_place(&mut buf);
        assert_eq!(buf, e);
        assert_eq!(a, b);
    }

    /// Q: does Stage 2 actually help on biased residuals, and stay neutral on unbiased ones?
    #[test]
    fn stage2_quality_effect() {
        let mut rng = Rng(99);
        for (name, off) in [("bias +6", 6i32), ("unbiased", 0)] {
            let e: Vec<i32> = (0..400_000).map(|_| ((rng.next() % 17) as i32 - 8) + off).collect();
            let mut r = e.clone();
            Stage2::new().encode_in_place(&mut r);
            let mad = |v: &[i32]| v.iter().map(|x| x.abs() as f64).sum::<f64>() / v.len() as f64;
            println!("{name}: H(e) = {:.4} H(r) = {:.4} bits; mean|e| = {:.3} mean|r| = {:.3}", entropy_i32(&e), entropy_i32(&r), mad(&e), mad(&r));
            assert!(mad(&r) <= mad(&e) + 0.05);
        }
    }

    /// Q: what are Stage 2 encode/decode throughputs (release mode)?
    #[test]
    #[ignore]
    fn stage2_throughput() {
        let mut rng = Rng(5);
        let n = 1 << 24;
        let e: Vec<i32> = (0..n).map(|_| (rng.next() % 61) as i32 - 30).collect();
        let mut buf = e.clone();
        let (mut te, mut td) = (f64::MAX, f64::MAX);
        for _ in 0..9 {
            buf.copy_from_slice(&e);
            let t = Instant::now();
            Stage2::new().encode_in_place(&mut buf);
            te = te.min(t.elapsed().as_secs_f64());
            let t = Instant::now();
            Stage2::new().decode_in_place(&mut buf);
            td = td.min(t.elapsed().as_secs_f64());
        }
        assert_eq!(buf, e);
        let nf = n as f64;
        println!("stage2 encode {:.3} ns/sample ({:.0} Msamples/s)  decode {:.3} ns/sample ({:.0} Msamples/s)", te / nf * 1e9, nf / te / 1e6, td / nf * 1e9, nf / td / 1e6);
    }

    // ---- Stage 3 ----

    use crate::rangecoder::{BinDec, BinEnc};

    /// Encoder-side IO that records (p, bit); with `cost` it also accumulates -log2 P(bit) in bits.
    struct RecIo {
        log: Vec<(u32, u32)>,
        bits: f64,
    }
    impl BitIo for RecIo {
        fn code(&mut self, p1: u32, bit: u32) -> u32 {
            self.log.push((p1, bit));
            let p = p1 as f64 / 65536.0;
            self.bits -= if bit == 1 { p.log2() } else { (1.0 - p).log2() };
            bit
        }
    }
    /// Decoder-side IO replaying a recorded bit log and asserting that every probability matches.
    struct ReplayIo<'a> {
        log: &'a [(u32, u32)],
        i: usize,
    }
    impl BitIo for ReplayIo<'_> {
        fn code(&mut self, p1: u32, _bit: u32) -> u32 {
            let (p, b) = self.log[self.i];
            assert_eq!(p, p1, "probability diverged at coded bit {}", self.i);
            self.i += 1;
            b
        }
    }
    /// Cost-only IO (no log): measures bits.
    struct CostIo(f64);
    impl BitIo for CostIo {
        #[inline(always)]
        fn code(&mut self, p1: u32, bit: u32) -> u32 {
            let p = p1 as f64 / 65536.0;
            self.0 -= if bit == 1 { p.log2() } else { (1.0 - p).log2() };
            bit
        }
    }
    /// Bit-supply IO for model-only decode timing (no probability check, no coder).
    struct BitsIo<'a> {
        bits: &'a [u8],
        i: usize,
    }
    impl BitIo for BitsIo<'_> {
        #[inline(always)]
        fn code(&mut self, _p1: u32, _bit: u32) -> u32 {
            let b = self.bits[self.i];
            self.i += 1;
            b as u32
        }
    }
    /// Model-only encode timing: accepts the bit without coding or recording.
    struct NullIo;
    impl BitIo for NullIo {
        #[inline(always)]
        fn code(&mut self, _p1: u32, bit: u32) -> u32 {
            bit
        }
    }
    struct BitLog(Vec<u8>);
    impl BitIo for BitLog {
        #[inline(always)]
        fn code(&mut self, _p1: u32, bit: u32) -> u32 {
            self.0.push(bit as u8);
            bit
        }
    }

    fn laplace(rng: &mut Rng, b: f64) -> i32 {
        let u = (rng.next() >> 11) as f64 / (1u64 << 53) as f64;
        let v = -b * (1.0 - u).ln();
        let v = v.round() as i32;
        if rng.next() & 1 == 0 { v } else { -v }
    }

    /// Exact entropy (bits) of the rounded two-sided Laplace(b) distribution.
    fn laplace_entropy(b: f64) -> f64 {
        let cdf = |x: f64| if x < 0.0 { 0.5 * (x / b).exp() } else { 1.0 - 0.5 * (-x / b).exp() };
        let kmax = (40.0 * b) as i32 + 40;
        let mut h = 0.0;
        for k in -kmax..=kmax {
            let p = cdf(k as f64 + 0.5) - cdf(k as f64 - 0.5);
            if p > 0.0 {
                h -= p * p.log2();
            }
        }
        h
    }

    fn model_bits(res: &[i32], w: usize, h: usize, bits: u32) -> f64 {
        let mut m = Stage3::new(w, h, bits).unwrap();
        let mut io = CostIo(0.0);
        m.encode_plane(res, &mut io).unwrap();
        io.0
    }

    /// Q: real range-coder round trip over the full expanded domain [-2M, 2M] for every bit depth
    /// (extremes, uniform, Laplace, flat); is the decoded plane identical and does the model end in the same state?
    #[test]
    fn stage3_roundtrip_full_range() {
        let mut rng = Rng(31337);
        for bits in 1..=16u32 {
            let rm = 2 * ((1i32 << bits) - 1);
            let (w, h) = (37, 29);
            for kind in 0..5 {
                let r: Vec<i32> = (0..w * h).map(|i| match kind {
                    0 => (rng.next() % (2 * rm as u64 + 1)) as i32 - rm,
                    1 => if i % 2 == 0 { rm } else { -rm },
                    2 => 0,
                    3 => laplace(&mut rng, 3.0).clamp(-rm, rm),
                    _ => if i % 97 == 0 { rm } else { 0 },
                }).collect();
                let mut enc = Stage3::new(w, h, bits).unwrap();
                let mut be = BinEnc::new();
                enc.encode_plane(&r, &mut be).unwrap();
                let bytes = be.finish();
                let mut dec = Stage3::new(w, h, bits).unwrap();
                let mut bd = BinDec::new(&bytes);
                let mut out = vec![0i32; w * h];
                dec.decode_plane(&mut out, &mut bd).unwrap();
                assert_eq!(out, r, "bits {bits} kind {kind}");
                assert!(enc == dec, "model state diverged: bits {bits} kind {kind}");
            }
            let mut m = Stage3::new(2, 2, bits).unwrap();
            assert_eq!(m.encode_plane(&[0, 0, 0, rm + 1], &mut NullIo), Err(Stage3Error::ResidualOutOfRange));
            assert_eq!(m.encode_plane(&[0; 3], &mut NullIo), Err(Stage3Error::BadLength));
        }
        assert_eq!(Stage3::new(1, 1, 0).err(), Some(Stage3Error::BadBits(0)));
        assert_eq!(Stage3::new(1, 1, 17).err(), Some(Stage3Error::BadBits(17)));
    }

    /// Q: does the decoder see bit-for-bit the same probabilities (contexts, mixing, updates) as the encoder,
    /// and is the whole thing deterministic across runs?
    #[test]
    fn stage3_probability_lockstep_and_determinism() {
        let mut rng = Rng(4242);
        let (w, h, bits) = (61, 47, 12);
        let r: Vec<i32> = (0..w * h).map(|i| laplace(&mut rng, if (i / w / 8) % 2 == 0 { 1.5 } else { 40.0 })).collect();
        let run = || {
            let mut m = Stage3::new(w, h, bits).unwrap();
            let mut io = RecIo { log: Vec::new(), bits: 0.0 };
            m.encode_plane(&r, &mut io).unwrap();
            (m, io.log)
        };
        let (m1, log1) = run();
        let (m2, log2) = run();
        assert_eq!(log1, log2);
        assert!(m1 == m2);
        let mut d = Stage3::new(w, h, bits).unwrap();
        let mut rp = ReplayIo { log: &log1, i: 0 };
        let mut out = vec![0i32; w * h];
        d.decode_plane(&mut out, &mut rp).unwrap();
        assert_eq!(out, r);
        assert_eq!(rp.i, log1.len());
        assert!(d == m1);
        // reset() must restore the exact initial state.
        let mut m3 = m1.clone();
        m3.reset();
        assert!(m3 == Stage3::new(w, h, bits).unwrap());
    }

    /// Q: does the full chain Stage 1 -> 3 directly (real range coder) and back reconstruct exactly?
    #[test]
    fn stage3_composed_stages_1_to_3() {
        let mut rng = Rng(2024);
        for bits in [1u32, 4, 8, 12, 16] {
            for &(w, h) in &SIZES {
                for kind in 0..6 {
                    let src = pattern(kind, w, h, bits, &mut rng);
                    let mut s1 = Stage1::new(w, h, bits).unwrap();
                    let mut res = vec![0i32; w * h];
                    s1.encode(&src, &mut res).unwrap();
                    let mut m = Stage3::new(w, h, bits).unwrap();
                    let mut be = BinEnc::new();
                    m.encode_plane(&res, &mut be).unwrap();
                    let bytes = be.finish();

                    let mut m = Stage3::new(w, h, bits).unwrap();
                    let mut bd = BinDec::new(&bytes);
                    let mut back = vec![0i32; w * h];
                    m.decode_plane(&mut back, &mut bd).unwrap();
                    let mut out = vec![0u16; w * h];
                    s1.decode(&back, &mut out).unwrap();
                    assert_eq!(out, src, "bits {bits} {w}x{h} kind {kind}");
                }
            }
        }
    }

    /// Q: how are fresh/unseen contexts handled? A fresh model must start at p ~ 1/2, and after training only on
    /// small residuals a never-seen huge residual in an unseen high-energy context must still code at bounded cost.
    #[test]
    fn stage3_sparse_and_unseen_contexts() {
        let mut m = Stage3::new(64, 64, 16).unwrap();
        let mut io = RecIo { log: Vec::new(), bits: 0.0 };
        m.code_sample(&mut io, 5);
        let (p0, _) = io.log[0];
        assert!((32768 - 64..=32768 + 64).contains(&p0), "fresh p = {p0}");

        let mut m = Stage3::new(64, 64, 16).unwrap();
        let mut io = CostIo(0.0);
        for _ in 0..640 {
            m.code_sample(&mut io, 0);
        }
        let mut io = CostIo(0.0);
        let before = io.0;
        m.code_sample(&mut io, 100_000);
        let cost = io.0 - before;
        println!("cost of a never-seen 100000 after 640 zeros: {cost:.1} bits (uniform over the domain = {:.1})", (4.0 * 65535.0f64 + 1.0).log2());
        assert!(cost < 60.0);
    }

    /// Q: does the model respond correctly to different statistics? (measured: model bits/sample vs exact entropy)
    #[test]
    fn stage3_synthetic_distributions() {
        let mut rng = Rng(8080);
        let (w, h) = (256, 256);
        let n = (w * h) as f64;
        println!("-- iid Laplace, model vs exact entropy (bits/sample) [measured / analytic]");
        for b in [0.5f64, 2.0, 16.0, 256.0] {
            let r: Vec<i32> = (0..w * h).map(|_| laplace(&mut rng, b)).collect();
            let bits = model_bits(&r, w, h, 16) / n;
            let hh = laplace_entropy(b);
            println!("  b = {b:>5}: model {bits:.4}  entropy {hh:.4}  overhead {:+.2}%", (bits / hh - 1.0) * 100.0);
            assert!(bits < hh * 1.06 + 0.02);
        }

        // Spatial scale switching: 32x32 blocks of b=1 / b=30. Compare to the oracle (knows the block scale) and to
        // the best static zero-order model (mixture entropy).
        let (bl, bh) = (1.0, 30.0);
        let r: Vec<i32> = (0..w * h).map(|i| {
            let (x, y) = (i % w, i / w);
            laplace(&mut rng, if (x / 32 + y / 32) % 2 == 0 { bl } else { bh })
        }).collect();
        let bits = model_bits(&r, w, h, 12) / n;
        let oracle = 0.5 * (laplace_entropy(bl) + laplace_entropy(bh));
        let mut hist = std::collections::HashMap::new();
        for &v in &r { *hist.entry(v).or_insert(0usize) += 1; }
        let order0 = -hist.values().map(|&c| { let p = c as f64 / n; c as f64 / n * p.log2() }).sum::<f64>();
        println!("-- scale-switching blocks: model {bits:.4}  oracle {oracle:.4}  static order-0 {order0:.4}");
        assert!(bits < order0 - 0.3, "contexts must beat static order-0");
        assert!(bits < oracle * 1.08);

        // Sign structure: identical magnitudes, random vs persistent signs (P(same as W) = 0.85).
        let mags: Vec<i32> = (0..w * h).map(|_| laplace(&mut rng, 6.0).abs() + 1).collect();
        let rand_s: Vec<i32> = mags.iter().map(|&a| if rng.next() & 1 == 0 { a } else { -a }).collect();
        let mut corr = rand_s.clone();
        let mut last = 1i32;
        for (i, v) in corr.iter_mut().enumerate() {
            let same = (rng.next() % 100) < 85;
            if i % w == 0 { last = if rng.next() & 1 == 0 { 1 } else { -1 }; }
            let sgn = if same { last } else { -last };
            *v = mags[i] * sgn;
            last = sgn;
        }
        let (b_rand, b_corr) = (model_bits(&rand_s, w, h, 12), model_bits(&corr, w, h, 12));
        let saved = (b_rand - b_corr) / n;
        let ideal = 1.0 + 0.85f64 * 0.85f64.log2() + 0.15 * 0.15f64.log2();
        println!("-- sign persistence 0.85: saved {saved:.4} bits/sample; ideal saving {ideal:.4}; random-sign cost vs 1 bit/sign: {:+.4} bits/sample", (b_rand - model_bits(&mags, w, h, 12)) / n - 1.0);
        assert!(saved > ideal * 0.8);

        // Flat plane and full-domain uniform.
        let zeros = vec![0i32; w * h];
        let z = model_bits(&zeros, w, h, 16) / n;
        let rm = 2 * 65535;
        let uni: Vec<i32> = (0..w * h).map(|_| (rng.next() % (2 * rm as u64 + 1)) as i32 - rm).collect();
        let u = model_bits(&uni, w, h, 16) / n;
        println!("-- all-zero plane: {z:.5} bits/sample; uniform over +-2M (B=16): {u:.3} vs log2(4M+1) = {:.3}", ((2 * rm + 1) as f64).log2());
        assert!(z < 0.01);
        assert!(u < ((2 * rm + 1) as f64).log2() + 0.15);
    }

    /// Q: Stage 3 model-only throughput (no coder), encode vs decode, plus memory. Residuals ~ Laplace(b=4).
    #[test]
    #[ignore]
    fn stage3_throughput_and_memory() {
        let mut rng = Rng(606);
        let (w, h) = (2048, 2048);
        let n = (w * h) as f64;
        let r: Vec<i32> = (0..w * h).map(|i| laplace(&mut rng, if (i / w / 64) % 2 == 0 { 2.0 } else { 8.0 })).collect();
        let mut m = Stage3::new(w, h, 8).unwrap();
        println!("memory: model state {} B ({} slots x4 + sign + {} mixer sets x{}), neighbour rows {} B, total {} B; struct {} B",
            m.model_state_bytes(), S3_SLOTS, m.wt.len(), S3_IN * 4, m.total_bytes() - m.model_state_bytes(), m.total_bytes(), std::mem::size_of::<Stage3>());
        let mut log = BitLog(Vec::new());
        m.encode_plane(&r, &mut log).unwrap();
        let bits = log.0;
        let (mut te, mut td, mut tc) = (f64::MAX, f64::MAX, f64::MAX);
        let mut out = vec![0i32; w * h];
        for _ in 0..5 {
            m.reset();
            let t = Instant::now();
            m.encode_plane(&r, &mut NullIo).unwrap();
            te = te.min(t.elapsed().as_secs_f64());
            m.reset();
            let mut io = BitsIo { bits: &bits, i: 0 };
            let t = Instant::now();
            m.decode_plane(&mut out, &mut io).unwrap();
            td = td.min(t.elapsed().as_secs_f64());
            m.reset();
            let mut be = BinEnc::new();
            let t = Instant::now();
            m.encode_plane(&r, &mut be).unwrap();
            tc = tc.min(t.elapsed().as_secs_f64());
            let bytes = be.finish();
            if tc == tc.min(f64::MAX) { std::hint::black_box(bytes.len()); }
        }
        assert_eq!(out, r);
        m.reset();
        let mut cio = CostIo(0.0);
        m.encode_plane(&r, &mut cio).unwrap();
        println!("coded bits/sample {:.3}, modelled binary decisions/sample {:.2}", cio.0 / n, bits.len() as f64 / n);
        println!("model encode {:.1} ns/sample ({:.1} Msamples/s)  model decode {:.1} ns/sample ({:.1} Msamples/s)  model+real range coder encode {:.1} ns/sample",
            te / n * 1e9, n / te / 1e6, td / n * 1e9, n / td / 1e6, tc / n * 1e9);
    }

    // ----------------------------------------------------------------------------------------------
    // Stage 4
    // ----------------------------------------------------------------------------------------------

    /// Random (p, bit) streams: bit drawn from p (typical), or adversarial (against p), at extreme and mid probabilities.
    fn s4_stream(rng: &mut Rng, n: usize, mode: usize) -> Vec<(u32, u32)> {
        (0..n)
            .map(|_| {
                let p = match mode {
                    0 => 32768,
                    1 => 32,
                    2 => 65503,
                    _ => (32 + rng.next() % (65503 - 32 + 1)) as u32,
                };
                let likely = (rng.next() & 0xFFFF) < p as u64;
                let bit = match mode {
                    1 | 2 => (rng.next() % 997 == 0) as u32 ^ (p > 32768) as u32, // mostly the likely value, rare surprises
                    _ => likely as u32,
                };
                (p, bit)
            })
            .collect()
    }

    fn s4_encode(stream: &[(u32, u32)]) -> Vec<u8> {
        let mut e = Stage4Enc::new();
        for &(p, b) in stream {
            e.code(p, b);
        }
        e.finish().to_vec()
    }

    fn s4_decode_check(bytes: &[u8], stream: &[(u32, u32)]) {
        let mut d = Stage4Dec::new(bytes);
        for (i, &(p, b)) in stream.iter().enumerate() {
            assert_eq!(d.code(p, 0), b, "bit {i}");
        }
    }

    /// Q: does the coder round-trip every bit for every probability regime, and is termination within budget?
    #[test]
    fn stage4_roundtrip_and_termination() {
        let mut rng = Rng(4040);
        for mode in 0..4 {
            for n in [0usize, 1, 2, 3, 7, 31, 100, 1000, 20000] {
                for _ in 0..4 {
                    let stream = s4_stream(&mut rng, n, mode);
                    let bytes = s4_encode(&stream);
                    s4_decode_check(&bytes, &stream);
                    // Same arithmetic as the shared BinEnc; only the termination differs, and it must never cost more.
                    let mut be = BinEnc::new();
                    for &(p, b) in &stream {
                        be.code(p, b);
                    }
                    let reference = be.finish();
                    assert!(bytes.len() <= reference.len(), "mode {mode} n {n}: {} > {}", bytes.len(), reference.len());
                }
            }
        }
        // A degenerate empty stream costs nothing.
        assert!(s4_encode(&[]).is_empty());
    }

    /// Q: carry propagation through long 0xFF runs (probability extremes, repeated surprises) stays exact.
    #[test]
    fn stage4_carry_stress() {
        let mut rng = Rng(77);
        for round in 0..200 {
            let n = 500 + (rng.next() % 4000) as usize;
            let stream: Vec<(u32, u32)> = (0..n)
                .map(|_| {
                    let p = if rng.next() & 1 == 0 { 65503 } else { 32 };
                    // Bias toward unlikely symbols: they shrink the range most and push `low` to its top.
                    let unlikely = rng.next() % (2 + round % 7) == 0;
                    (p, if (p > 32768) ^ unlikely { 1 } else { 0 })
                })
                .collect();
            let bytes = s4_encode(&stream);
            s4_decode_check(&bytes, &stream);
        }
    }

    /// Q: is reuse (reset) byte-identical to a fresh coder?
    #[test]
    fn stage4_reset_and_determinism() {
        let mut rng = Rng(5);
        let a = s4_stream(&mut rng, 5000, 3);
        let b = s4_stream(&mut rng, 7000, 3);
        let mut e = Stage4Enc::new();
        for &(p, x) in &a {
            e.code(p, x);
        }
        e.finish();
        e.reset();
        for &(p, x) in &b {
            e.code(p, x);
        }
        assert_eq!(e.finish(), &s4_encode(&b)[..]);
        assert_eq!(s4_encode(&a), s4_encode(&a));
    }

    /// Q: does the complete Stage 1-4 chain reconstruct exactly (multi-plane, both model-carry modes, every bit depth)?
    #[test]
    fn stage4_composed_stages_1_to_4() {
        let mut rng = Rng(2025);
        for bits in [1u32, 4, 8, 12, 16] {
            for &(w, h) in &SIZES {
                for carry in [false, true] {
                    let planes: Vec<Vec<u16>> = (0..3).map(|k| pattern(k * 2, w, h, bits, &mut rng)).collect();
                    let refs: Vec<&[u16]> = planes.iter().map(|p| p.as_slice()).collect();
                    let mut codec = EllieCodec::new(w, h, bits).unwrap();
                    codec.carry_model = carry;
                    let mut enc = Stage4Enc::new();
                    codec.encode_planes(&refs, &mut enc).unwrap();
                    let bytes = enc.finish().to_vec();

                    // Reusing the same codec object for a second identical encode must reproduce the bytes.
                    enc.reset();
                    codec.encode_planes(&refs, &mut enc).unwrap();
                    assert_eq!(enc.finish(), &bytes[..], "bits {bits} {w}x{h} carry {carry}");

                    let mut out: Vec<Vec<u16>> = vec![vec![0; w * h]; 3];
                    let mut outs: Vec<&mut [u16]> = out.iter_mut().map(|p| p.as_mut_slice()).collect();
                    let mut dec = EllieCodec::new(w, h, bits).unwrap();
                    dec.carry_model = carry;
                    dec.decode_planes(&bytes, &mut outs).unwrap();
                    assert_eq!(out, planes, "bits {bits} {w}x{h} carry {carry}");
                }
            }
        }
    }

    /// Q: corrupt or truncated streams must never panic (they may decode to wrong samples or report an error).
    #[test]
    fn stage4_garbage_does_not_panic() {
        let mut rng = Rng(9);
        let (w, h, bits) = (17, 13, 8);
        let plane = pattern(2, w, h, bits, &mut rng);
        let mut codec = EllieCodec::new(w, h, bits).unwrap();
        let mut enc = Stage4Enc::new();
        codec.encode_planes(&[&plane], &mut enc).unwrap();
        let good = enc.finish().to_vec();
        for cut in [0, 1, good.len() / 2, good.len().saturating_sub(1)] {
            let mut out = vec![0u16; w * h];
            let _ = codec.decode_planes(&good[..cut], &mut [&mut out[..]]);
        }
        for _ in 0..50 {
            let junk: Vec<u8> = (0..64).map(|_| rng.next() as u8).collect();
            let mut out = vec![0u16; w * h];
            let _ = codec.decode_planes(&junk, &mut [&mut out[..]]);
        }
    }

    /// Encoder that special-cases p = 1/2 with a shift instead of the multiply (A/B for the equiprobable fast path).
    struct HalfShiftEnc(Stage4Enc);
    impl BitIo for HalfShiftEnc {
        #[inline(always)]
        fn code(&mut self, p1: u32, bit: u32) -> u32 {
            if p1 != 32768 {
                return self.0.code(p1, bit);
            }
            let e = &mut self.0;
            let bound = e.range >> 1;
            if bit != 0 {
                e.range = bound;
            } else {
                e.low += bound as u64;
                e.range -= bound;
            }
            while e.range < S4_TOP {
                e.range <<= 8;
                e.shift_low();
            }
            bit
        }
    }

    /// Q: Stage 4 cost on top of the model, and is a p = 1/2 shortcut worth a branch? Laplace residuals, 8 bit.
    #[test]
    #[ignore]
    fn stage4_throughput_and_overhead() {
        let mut rng = Rng(808);
        let (w, h) = (2048, 2048);
        let n = (w * h) as f64;
        let r: Vec<i32> = (0..w * h).map(|i| laplace(&mut rng, if (i / w / 64) % 2 == 0 { 2.0 } else { 12.0 })).collect();
        let mut m = Stage3::new(w, h, 8).unwrap();
        let mut cio = CostIo(0.0);
        m.encode_plane(&r, &mut cio).unwrap();
        let ideal = cio.0 / 8.0;
        let (mut t_null, mut t_s4, mut t_br, mut t_dec) = (f64::MAX, f64::MAX, f64::MAX, f64::MAX);
        let mut bytes = Vec::new();
        let mut out = vec![0i32; w * h];
        for _ in 0..5 {
            m.reset();
            let t = Instant::now();
            m.encode_plane(&r, &mut NullIo).unwrap();
            t_null = t_null.min(t.elapsed().as_secs_f64());
            m.reset();
            let mut e = Stage4Enc::with_capacity(w * h);
            let t = Instant::now();
            m.encode_plane(&r, &mut e).unwrap();
            let l = e.finish().len();
            t_s4 = t_s4.min(t.elapsed().as_secs_f64());
            bytes = e.finish().to_vec();
            assert_eq!(l, bytes.len());
            m.reset();
            let mut e = HalfShiftEnc(Stage4Enc::with_capacity(w * h));
            let t = Instant::now();
            m.encode_plane(&r, &mut e).unwrap();
            let l2 = e.0.finish().len();
            t_br = t_br.min(t.elapsed().as_secs_f64());
            assert_eq!(l2, l, "shift path must be bit-identical");
            m.reset();
            let mut d = Stage4Dec::new(&bytes);
            let t = Instant::now();
            m.decode_plane(&mut out, &mut d).unwrap();
            t_dec = t_dec.min(t.elapsed().as_secs_f64());
        }
        assert_eq!(out, r);
        println!("ideal model bytes {:.0}, coded bytes {}, overhead {:.0} B ({:.4}%)", ideal, bytes.len(), bytes.len() as f64 - ideal, (bytes.len() as f64 / ideal - 1.0) * 100.0);
        println!(
            "ns/sample: model only {:.1}, model+stage4 enc {:.1}, +half-shift variant {:.1}, model+stage4 dec {:.1}",
            t_null / n * 1e9, t_s4 / n * 1e9, t_br / n * 1e9, t_dec / n * 1e9
        );
    }

    /// Bc255S11: encoder and decoder models agree on every coded probability and end in identical state; the profile
    /// really differs from Legacy4; and reset() restores the exact initial state.
    #[test]
    fn stage3_bc_profile_lockstep_determinism_and_distinct_from_legacy() {
        let mut rng = Rng(777);
        for &(w, h, bits) in &[(61usize, 47usize, 12u32), (33, 20, 8), (17, 9, 9), (5, 40, 16)] {
            let r: Vec<i32> = (0..w * h).map(|i| laplace(&mut rng, if (i / w / 8) % 2 == 0 { 1.5 } else { 40.0 }).clamp(-2 * ((1 << bits) - 1), 2 * ((1 << bits) - 1))).collect();
            let run = |p| {
                let mut m = Stage3::with_profile(w, h, bits, p).unwrap();
                let mut io = RecIo { log: Vec::new(), bits: 0.0 };
                m.encode_plane(&r, &mut io).unwrap();
                (m, io.log)
            };
            let (m1, log1) = run(Stage3Profile::Bc255S11);
            let (m2, log2) = run(Stage3Profile::Bc255S11);
            assert_eq!(log1, log2);
            assert!(m1 == m2);
            let mut d = Stage3::with_profile(w, h, bits, Stage3Profile::Bc255S11).unwrap();
            let mut rp = ReplayIo { log: &log1, i: 0 };
            let mut out = vec![0i32; w * h];
            d.decode_plane(&mut out, &mut rp).unwrap();
            assert_eq!(out, r);
            assert_eq!(rp.i, log1.len());
            assert!(d == m1);
            let mut m3 = m1.clone();
            m3.reset();
            assert!(m3 == Stage3::with_profile(w, h, bits, Stage3Profile::Bc255S11).unwrap());
            assert_ne!(log1, run(Stage3Profile::Legacy4).1, "profiles must give different probability streams");
        }
    }

    /// Bc255S11 only touches contexts B and C: slots of A and D stay at their initial value.
    #[test]
    fn stage3_bc_profile_leaves_contexts_a_and_d_untouched() {
        let mut rng = Rng(5);
        let (w, h) = (40, 30);
        let r: Vec<i32> = (0..w * h).map(|_| laplace(&mut rng, 6.0)).collect();
        let mut m = Stage3::with_profile(w, h, 8, Stage3Profile::Bc255S11).unwrap();
        m.encode_plane(&r, &mut NullIo).unwrap();
        assert!(m.t[OFF_A..OFF_B].iter().all(|&s| s == SLOT_INIT));
        assert!(m.t[OFF_D..OFF_D + 64].iter().all(|&s| s == SLOT_INIT));
        assert!(m.t[OFF_B..OFF_C].iter().any(|&s| s != SLOT_INIT));
        assert!(m.t[OFF_C..OFF_D].iter().any(|&s| s != SLOT_INIT));
        assert!(m.t.iter().all(|&s| s & 1023 <= 255));
    }

    /// Full Stage 1-4 chain with a real range coder under both profiles, across bit depths.
    #[test]
    fn ellie_codec_round_trips_under_both_profiles() {
        for profile in [Stage3Profile::Legacy4, Stage3Profile::Bc255S11, Stage3Profile::Bc255S11Direct] {
            for bits in [1u32, 8, 9, 12, 16] {
                let (w, h) = (23, 17);
                let mut rng = Rng(bits as u64 * 31 + 1);
                let maxv = (1u64 << bits) - 1;
                let plane: Vec<u16> = (0..w * h).map(|i| (((i * 7) as u64 + rng.next() % 9) % (maxv + 1)) as u16).collect();
                let mut enc = Stage4Enc::new();
                EllieCodec::with_profile(w, h, bits, profile).unwrap().encode_planes(&[&plane], &mut enc).unwrap();
                let bytes = enc.finish().to_vec();
                let mut out = vec![0u16; w * h];
                EllieCodec::with_profile(w, h, bits, profile).unwrap().decode_planes(&bytes, &mut [out.as_mut_slice()]).unwrap();
                assert_eq!(out, plane, "{profile:?} bits {bits}");
            }
        }
    }
}
