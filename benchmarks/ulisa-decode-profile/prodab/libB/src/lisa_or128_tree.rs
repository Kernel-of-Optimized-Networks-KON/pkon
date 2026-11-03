//! OR-128 context tree (Stage 3 rule version 3): the learned successor of the fixed 128-entry table in `lisa_or128`.
//!
//! The causal neighborhood (W, N, NW zigzag residual, `None` = outside the plane or not yet available) is turned into
//! `NF` small integer features; a frozen binary decision tree (`lisa_or128_tree_data`, learned greedily from a single
//! leaf by `exp_or128_tree`) maps them to a context id, one id per leaf. Nothing is stored in the stream: the decoder
//! recomputes every id from already decoded residuals. Missing neighbors are imputed exactly as in `lisa_or128`.
use crate::lisa::unzigzag;
use crate::lisa_or128::level;
pub use crate::lisa_or128_tree_data::{OR128_TREE_LEAVES, OR128_TREE_NODES, OR128_TREE_ROOT};

pub const NF: usize = 18;
/// Feature names, index-aligned with [`features`].
pub const FEATURE_NAMES: [&str; NF] = [
    "lvl(W)", "lvl(N)", "lvl(NW)", "lvl(max)", "lvl(min)", "lvl(med)", "lvl(mean)", "lvl(|W-N|)", "lvl(|N-NW|)",
    "lvl(|W-NW|)", "lvl(W+N)", "lvl(max-min)", "sign(W)", "sign(N)", "sign(NW)", "missing", "lvl(|sW+sN-sNW|)",
    "lvl(|sW+sN|)",
];
const LEAF: u16 = 0x8000;

#[inline]
fn lv(x: u64) -> u8 {
    level(x.min(u64::from(u32::MAX)) as u32) as u8
}

/// Features of the causal neighborhood. `sX` below is the signed residual `unzigzag(X)`; sign features are the zigzag
/// parity (1 = negative). Magnitude features are half-octave levels (`lisa_or128::level`, 0..=21).
#[inline]
pub fn features(left: Option<u32>, above: Option<u32>, above_left: Option<u32>) -> [u8; NF] {
    let nb = [left, above, above_left];
    let (mut sum, mut n) = (0u64, 0u64);
    for v in nb.into_iter().flatten() {
        sum += u64::from(v);
        n += 1;
    }
    let fill = if n == 0 { 0 } else { ((sum + n / 2) / n) as u32 };
    let [w, nn, nw] = nb.map(|v| v.unwrap_or(fill));
    let (w64, n64, nw64) = (u64::from(w), u64::from(nn), u64::from(nw));
    let (mx, mn) = (w64.max(n64).max(nw64), w64.min(n64).min(nw64));
    let med = w64 + n64 + nw64 - mx - mn;
    let mean = (w64 + n64 + nw64 + 1) / 3;
    let (sw, sn, snw) = (i64::from(unzigzag(w)), i64::from(unzigzag(nn)), i64::from(unzigzag(nw)));
    [
        lv(w64),
        lv(n64),
        lv(nw64),
        lv(mx),
        lv(mn),
        lv(med),
        lv(mean),
        lv(w64.abs_diff(n64)),
        lv(n64.abs_diff(nw64)),
        lv(w64.abs_diff(nw64)),
        lv(w64 + n64),
        lv(mx - mn),
        (w & 1) as u8,
        (nn & 1) as u8,
        (nw & 1) as u8,
        (3 - n) as u8,
        lv((sw + sn - snw).unsigned_abs()),
        lv((sw + sn).unsigned_abs()),
    ]
}

/// Walks a tree given as `(root, nodes)`; returns the leaf id.
#[inline]
pub fn eval(root: u16, nodes: &[(u8, u8, u16, u16)], f: &[u8; NF]) -> u8 {
    let mut r = root;
    while r & LEAF == 0 {
        let (feat, thr, lo, hi) = nodes[usize::from(r)];
        r = if f[usize::from(feat)] <= thr { lo } else { hi };
    }
    (r & !LEAF) as u8
}

/// OR-128 tree context id (`0..OR128_TREE_LEAVES`). Total over all inputs.
#[inline]
pub fn context(left: Option<u32>, above: Option<u32>, above_left: Option<u32>) -> u8 {
    eval(OR128_TREE_ROOT, OR128_TREE_NODES, &features(left, above, above_left))
}

/// Context id of the 64-leaf truncation (Stage 3 rule version 4).
#[inline]
pub fn context64(left: Option<u32>, above: Option<u32>, above_left: Option<u32>) -> u8 {
    use crate::lisa_or128_tree_small_data::l64::*;
    eval(OR128_TREE_ROOT, OR128_TREE_NODES, &features(left, above, above_left))
}

/// Context id of the 32-leaf truncation (Stage 3 rule version 5).
#[inline]
pub fn context32(left: Option<u32>, above: Option<u32>, above_left: Option<u32>) -> u8 {
    use crate::lisa_or128_tree_small_data::l32::*;
    eval(OR128_TREE_ROOT, OR128_TREE_NODES, &features(left, above, above_left))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frozen_tree_is_well_formed() {
        assert!((1..=255).contains(&OR128_TREE_LEAVES));
        assert_eq!(OR128_TREE_NODES.len() + 1, OR128_TREE_LEAVES);
        let mut seen = vec![false; OR128_TREE_LEAVES];
        let mut stack = vec![OR128_TREE_ROOT];
        let mut visited = 0;
        while let Some(r) = stack.pop() {
            if r & LEAF != 0 {
                let id = usize::from(r & !LEAF);
                assert!(id < OR128_TREE_LEAVES && !seen[id]);
                seen[id] = true;
            } else {
                let (f, _, lo, hi) = OR128_TREE_NODES[usize::from(r)];
                assert!(usize::from(f) < NF);
                visited += 1;
                stack.extend([lo, hi]);
            }
        }
        assert!(seen.iter().all(|&s| s));
        assert_eq!(visited, OR128_TREE_NODES.len());
    }

    #[test]
    fn context_is_total_and_in_range() {
        let vals = [None, Some(0), Some(1), Some(2), Some(7), Some(1000), Some(u32::MAX)];
        for &a in &vals {
            for &b in &vals {
                for &c in &vals {
                    assert!(usize::from(context(a, b, c)) < OR128_TREE_LEAVES);
                }
            }
        }
    }
}
