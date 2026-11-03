//! EXPERIMENT (isolated): "probability cloud" residual model, an alternative to LISA Stage 3 + Stage 4.
//!
//! Nothing here is used by production LISA; it only consumes the public Stage 1/2 API and the PKON binary range coder.
//!
//! Model (context-free, one adaptive empirical distribution over all residuals, in Stage 2 traversal order):
//!   z   = zigzag residual (reuses `lisa::zigzag`)
//!   P_t(k) = (c_t(k) + a) / (N_t + a*K),  a = 1/2,  K = MODEL_K + 1 = 1025 symbols (z = 0..1023, plus ESC = 1024)
//!   z >= MODEL_K is coded as ESC followed by the exact tail v = z - MODEL_K (Elias-gamma-like: adaptive unary bit length
//!   of v+1, then the bits below the leading one at p = 1/2).
//! The distribution is realized as a complete binary tree over 2048 padded leaves. Each node holds the integer weight
//! W = 2*c_sum + (number of real leaves below) (so a = 1/2 exactly). A symbol is coded as 11 binary decisions from the root;
//! P(bit=1) = W_right / (W_left + W_right), quantized to 16 bits and clamped to [1, 65535]. A decision whose one child
//! holds only padding leaves (weight 0) is deterministic and not coded. The tree is updated with the actual symbol after
//! it is coded, identically in encoder and decoder.
//!
//! Stream: 26-byte header (magic, version, mode, K, a_num, a_den, residual count, payload length) + range-coder payload.
//! Image geometry/bit depth/CFA are NOT in the stream (same as the production LISA accounting).

use crate::lisa::*;
use crate::rangecoder::{BinDec, BinEnc, Bit, BitIo};

pub const MAGIC: [u8; 4] = *b"PCLD";
pub const VERSION: u8 = 1;
pub const MODE_GLOBAL_EMPIRICAL: u8 = 1;
pub const MODEL_K: usize = 1024;
pub const HEADER_LEN: usize = 4 + 1 + 1 + 2 + 1 + 1 + 8 + 8;
const ESC: usize = MODEL_K;
const LEAVES: usize = 2048;
const DEPTH: usize = 11;
const E_MAX: usize = 33;

/// Ideal-cost accounting gathered by the encoder (bits).
#[derive(Debug, Clone, Default)]
pub struct CloudStats {
    pub n: u64,
    pub escapes: u64,
    /// sum of -log2 of the probability of each tree decision actually coded (ideal, before range-coder rounding)
    pub tree_bits: f64,
    /// ideal bits spent in escape tails (adaptive unary part + p=1/2 mantissa)
    pub tail_bits: f64,
}

struct Cloud {
    w: Vec<u64>,
    unary: [Bit; E_MAX + 1],
}

impl Cloud {
    fn new() -> Self {
        let mut w = vec![0u64; 2 * LEAVES];
        for i in 0..=MODEL_K {
            w[LEAVES + i] = 1;
        }
        for i in (1..LEAVES).rev() {
            w[i] = w[2 * i] + w[2 * i + 1];
        }
        Cloud { w, unary: [Bit::NEW; E_MAX + 1] }
    }

    /// Codes the tree symbol (0..=ESC). Encoders pass the symbol; decoders pass 0 and receive the decoded one.
    fn code_sym<I: BitIo>(&mut self, io: &mut I, sym: usize, stats: Option<&mut CloudStats>) -> usize {
        let mut node = 1usize;
        let mut bits = 0f64;
        for lvl in 0..DEPTH {
            let (wl, wr) = (self.w[2 * node], self.w[2 * node + 1]);
            let want = ((sym >> (DEPTH - 1 - lvl)) & 1) as u32;
            let bit = if wr == 0 {
                0
            } else if wl == 0 {
                1
            } else {
                let p1 = ((wr << 16) / (wl + wr)).clamp(1, 65535) as u32;
                let b = io.code(p1, want);
                bits -= (if b == 1 { p1 as f64 } else { (65536 - p1) as f64 } / 65536.0).log2();
                b
            };
            node = 2 * node + bit as usize;
        }
        let leaf = node - LEAVES;
        let mut n = node;
        while n >= 1 {
            self.w[n] += 2;
            n /= 2;
        }
        if let Some(s) = stats {
            s.tree_bits += bits;
        }
        leaf
    }

    /// Escape tail for v >= 0. Returns the (de)coded v.
    fn code_tail<I: BitIo>(&mut self, io: &mut I, v: u32, stats: Option<&mut CloudStats>) -> Result<u32, LisaError> {
        let x = u64::from(v) + 1;
        let e_in = (64 - x.leading_zeros()) as usize;
        let mut cost = 0f64;
        let mut e = E_MAX;
        for i in 1..E_MAX {
            let b = &mut self.unary[i];
            let p1 = b.p1();
            let bit = io.code(p1, (e_in > i) as u32);
            cost -= (if bit == 1 { p1 as f64 } else { (65536 - p1) as f64 } / 65536.0).log2();
            b.adapt(bit);
            if bit == 0 {
                e = i;
                break;
            }
        }
        let mut out = 1u64;
        for j in (0..e - 1).rev() {
            out = (out << 1) | u64::from(io.code(32768, ((x >> j) & 1) as u32));
            cost += 1.0;
        }
        if let Some(s) = stats {
            s.tail_bits += cost;
        }
        u32::try_from(out - 1).map_err(|_| LisaError::CorruptStream("escape tail exceeds 32 bits"))
    }

    /// Shared encode/decode of one zigzag residual.
    fn code<I: BitIo>(&mut self, io: &mut I, z: u32, bd: u8, mut stats: Option<&mut CloudStats>) -> Result<u32, LisaError> {
        let sym = if (z as usize) < MODEL_K { z as usize } else { ESC };
        let got = self.code_sym(io, sym, stats.as_deref_mut());
        let z = if got < ESC {
            got as u32
        } else {
            if let Some(s) = stats.as_deref_mut() {
                s.escapes += 1;
            }
            let v = self.code_tail(io, z.wrapping_sub(MODEL_K as u32), stats.as_deref_mut())?;
            (MODEL_K as u32).checked_add(v).ok_or(LisaError::CorruptStream("escaped residual overflows"))?
        };
        if u64::from(z) > (2u64 << bd) - 2 {
            return Err(LisaError::CorruptStream("residual outside the bit-depth range"));
        }
        Ok(z)
    }
}

fn streams(data: &OrganizedResiduals) -> Vec<&[u32]> {
    match data {
        OrganizedResiduals::Morton(r) => vec![r.as_slice()],
        OrganizedResiduals::BayerPlanes(p) => p.iter().map(|v| v.as_slice()).collect(),
    }
}

/// Encodes the Stage 2 residuals (traversal order = Stage 2 data order: Morton, or Bayer planes 0..3 row-major).
pub fn encode_cloud(stage2: &Stage2Output) -> Result<(Vec<u8>, usize, CloudStats), LisaError> {
    let bd = stage2.bit_depth();
    let mut cloud = Cloud::new();
    let mut enc = BinEnc::new();
    let mut stats = CloudStats::default();
    for s in streams(stage2.data()) {
        for &z in s {
            cloud.code(&mut enc, z, bd, Some(&mut stats))?;
            stats.n += 1;
        }
    }
    let payload = enc.finish();
    let mut out = Vec::with_capacity(HEADER_LEN + payload.len());
    out.extend_from_slice(&MAGIC);
    out.push(VERSION);
    out.push(MODE_GLOBAL_EMPIRICAL);
    out.extend_from_slice(&(MODEL_K as u16).to_le_bytes());
    out.extend_from_slice(&[1, 2]); // pseudocount a = 1/2
    out.extend_from_slice(&stats.n.to_le_bytes());
    out.extend_from_slice(&(payload.len() as u64).to_le_bytes());
    out.extend_from_slice(&payload);
    Ok((out, HEADER_LEN, stats))
}

/// Decodes a cloud stream to Stage 1 residuals (Morton order). `metadata` and `bit_depth` are supplied externally,
/// exactly as for production LISA.
pub fn decode_cloud(stream: &[u8], metadata: &ImageMetadata, bit_depth: u8) -> Result<Stage1Output, LisaError> {
    if stream.len() < HEADER_LEN {
        return Err(LisaError::CorruptStream("cloud stream shorter than header"));
    }
    if stream[..4] != MAGIC {
        return Err(LisaError::CorruptStream("bad cloud magic"));
    }
    if stream[4] != VERSION || stream[5] != MODE_GLOBAL_EMPIRICAL {
        return Err(LisaError::CorruptStream("unsupported cloud version or mode"));
    }
    if u16::from_le_bytes([stream[6], stream[7]]) as usize != MODEL_K || stream[8] != 1 || stream[9] != 2 {
        return Err(LisaError::CorruptStream("unsupported cloud model parameters"));
    }
    let count = u64::from_le_bytes(stream[10..18].try_into().unwrap());
    let plen = u64::from_le_bytes(stream[18..26].try_into().unwrap());
    metadata.validate()?;
    let n = validate(metadata.width, metadata.height, bit_depth)?;
    if count != n as u64 {
        return Err(LisaError::CorruptStream("residual count disagrees with geometry"));
    }
    if plen != (stream.len() - HEADER_LEN) as u64 {
        return Err(LisaError::CorruptStream("payload length disagrees with stream length"));
    }
    let payload = &stream[HEADER_LEN..];

    // Layout template from public API: organize the identity index sequence 0..n; the result says where every Morton
    // index lands in the Stage 2 data order. No image data is involved.
    let idx = Stage1Output { residuals: (0..n as u32).collect(), metadata: metadata.clone(), bit_depth };
    let tmpl = organize_stage2(idx)?;
    let order: Vec<u32> = streams(tmpl.data()).into_iter().flatten().copied().collect();

    let mut cloud = Cloud::new();
    let mut dec = BinDec::new(payload);
    let mut residuals = vec![0u32; n];
    for &morton_idx in &order {
        residuals[morton_idx as usize] = cloud.code(&mut dec, 0, bit_depth, None)?;
    }
    if dec.consumed() != payload.len() {
        return Err(LisaError::CorruptStream("payload truncated or has trailing bytes"));
    }
    Ok(Stage1Output { residuals, metadata: metadata.clone(), bit_depth })
}
