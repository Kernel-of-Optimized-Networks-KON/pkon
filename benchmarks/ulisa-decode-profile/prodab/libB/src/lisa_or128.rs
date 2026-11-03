//! OR-128 context quantization (production Stage 3 rule version 2): 128 contexts chosen from the causal neighborhood.
//!
//! Each causal neighbor (W, N, NW zigzag residual) is mapped to a half-octave level; a missing neighbor is imputed with
//! the rounded mean of the available ones (all missing => 0). The ordered level triple indexes a frozen table
//! (`lisa_or128_table`) that yields the context id. Nothing is stored in the stream: the decoder recomputes every id from
//! already decoded residuals. (OR-128 is the production name of the PCQ-128 mapping developed in `lisa_pcq`.)
pub use crate::lisa_or128_table::{OR128_K, OR128_TABLE};

pub const LEVELS: usize = 22;
pub const KEYS: usize = LEVELS * LEVELS * LEVELS;
const _: () = assert!(OR128_TABLE.len() == KEYS);

/// Half-octave magnitude level: 0,1,2,3 exact; then {4-5, 6-7, 8-11, 12-15, 16-23, ...}; capped at LEVELS-1.
#[inline]
pub fn level(v: u32) -> usize {
    if v < 4 {
        return v as usize;
    }
    let l = 31 - v.leading_zeros() as usize;
    let half = ((v >> (l - 1)) & 1) as usize;
    (4 + 2 * (l - 2) + half).min(LEVELS - 1)
}

/// Table index of the causal neighborhood (`None` = outside the plane or not yet available).
#[inline]
pub fn key(left: Option<u32>, above: Option<u32>, above_left: Option<u32>) -> usize {
    let nb = [left, above, above_left];
    let (mut sum, mut n) = (0u64, 0u64);
    for v in nb.into_iter().flatten() {
        sum += u64::from(v);
        n += 1;
    }
    let fill = if n == 0 { 0 } else { ((sum + n / 2) / n) as u32 };
    let [a, b, c] = nb.map(|v| level(v.unwrap_or(fill)));
    (a * LEVELS + b) * LEVELS + c
}

/// OR-128 context id (`0..OR128_K`). Total over all inputs.
#[inline]
pub fn context(left: Option<u32>, above: Option<u32>, above_left: Option<u32>) -> u8 {
    OR128_TABLE[key(left, above, above_left)]
}
