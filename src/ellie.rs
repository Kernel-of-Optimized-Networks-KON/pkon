//! ELLIE: lossless image compression, one source file for all four stages.
//!
//! Stage 1 (this file, so far): prediction and residual generation.
//! Stages 2 (adaptive processing), 3 (probability modelling) and 4 (entropy coding) are not implemented yet.
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

/// Mixed prediction from the eight-neighbour-derived quantities. Exact integer arithmetic, one division.
#[inline(always)]
fn predict(m: i32, w: i32, ww: i32, n: i32, nn: i32, nw: i32, ne: i32, s_h: u32, s_v: u32, s45: u32, s135: u32) -> i32 {
    let mn = s45.min(s135);
    let s_d = mn + (mn >> 2);
    let smax = s_h.max(s_v).max(s_d);
    let sh = (32 - (smax | 1).leading_zeros()).saturating_sub(10);
    let sq = |s: u32| {
        let r = ((s >> sh) + 1) as u64;
        r * r
    };
    let (qh, qv, qd) = (sq(s_h), sq(s_v), sq(s_d));
    let (ph, pv, pd) = (qv * qd, qh * qd, qh * qv);
    let d = ph + pv + pd;

    let f = 2 + (s135 >= 2 * s45) as i32 - (s45 >= 2 * s135) as i32;
    let o = 4 * m;
    let h4 = (5 * w - ww + o) as u64;
    let v4 = (5 * n - nn + o) as u64;
    let d4 = ((4 - f) * nw + f * ne + o) as u64;

    let t = (ph * h4 + pv * v4 + pd * d4) / d;
    (((t + 2) >> 2) as i64 - m as i64).clamp(0, m as i64) as i32
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
                let pr = predict(m, n, n, n, nn, n, ne, s_h, s_v, s45, s135);
                let xv = resolve(base, pr as u32);
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
                let pr = predict(m, wv, wwv, n, nn, nw, ne, s_h, s_v, s45, s135);
                let xv = resolve(base + x, pr as u32) as i32;
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
                let p = predict(m, wv, wwv, n, nn, nw, ne, s_h, s_v, s45, s135);
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
            let h4 = (5 * w - ww) as i128 + o;
            let v4 = (5 * n - nn) as i128 + o;
            let d4 = (4 - f) * nw as i128 + f * ne as i128 + o;
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
                let got = predict(m, nb[0], nb[1], nb[2], nb[3], nb[4], nb[5], a, b, c, d);
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
                let got = predict(m, nb[0], nb[1], nb[2], nb[3], nb[4], nb[5], s[0], s[1], s[2], s[3]);
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
                assert_eq!(predict(m, v, v, v, v, v, v, 0, 0, 0, 0), v);
            }
        }
        // Equal weights (all S equal), predictors 3/4-valued: P4 mean = 4*value => exact.
        assert_eq!(predict(255, 10, 10, 10, 10, 10, 10, 5, 5, 5, 5), 10);
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
        assert_eq!(fnv(&a), 3390715494612369011, "regression pin");
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
}
