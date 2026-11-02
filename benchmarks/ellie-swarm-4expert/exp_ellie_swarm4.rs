//! ELLIE four-expert predictor swarm (H, V, D' without Paeth, independent Paeth), actual-bytes experiment. Isolated: production
//! predictor / probability model / range coder are untouched. The Stage 1 scan is copied here (it is private) and extended; the
//! residual planes it produces are coded by the PRODUCTION `Stage3` (Bc255S11Direct) and `Stage4Enc`, exactly as the production
//! codec does, so every size below is a real stream size. The container overhead is taken from the production file.
//!
//! Configs: `prod-copy` (3-expert copy of production; must reproduce the production prediction plane and bytes exactly),
//! `P4-prodrel` (4 experts, production-style Shrub weighting), `P4-shared` (4 experts, shared reliability g_i),
//! `P4-shared-tree5` / `-tree9` (shared reliability x pixel-only leaf modulation), `P4-shared-Lpx` (ablation: Paeth local
//! evidence at pixel scale instead of the proposed 4x-scale calibration).
//!   cargo run --release --bin exp_ellie_swarm4 -- --out <new dir> [--reps N]
use nanoscope::canon::{Canonical, Spec};
use nanoscope::ellie::{self, Stage1, Stage3, Stage3Profile, Stage4Dec, Stage4Enc};
use nanoscope::pkon::{self, CodecParams, EllieRepr, ELLIE_REPR_YCOCG_R};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::time::Instant;

const CROP: u32 = 1024;
const C_REL: i64 = 16; // stabilising constant c (x16 fixed point, same as production Shrub)
const G_MIN_SHARED: u64 = 4; // g_min of the shared reliability (production floor is 1)
const RATE_PROD: u32 = 3; // production Shrub rate 1/8
const RATE_SHARED: u32 = 5; // "slow" running mean: 1/32

#[derive(Clone, Copy, PartialEq)]
enum Rel {
    Prod,
    Shared,
}
#[derive(Clone, Copy, PartialEq)]
enum Tree {
    None,
    L5,
    L9,
}
#[derive(Clone, Copy)]
struct Cfg {
    name: &'static str,
    base: bool, // 3-expert production copy
    rel: Rel,
    tree: Tree,
    lshift: u32, // Paeth local evidence L = (2eW + 2eN + eNW + eNE) >> lshift ; the proposed calibration is 1
}
const CFGS: [Cfg; 6] = [
    Cfg { name: "prod-copy", base: true, rel: Rel::Prod, tree: Tree::None, lshift: 1 },
    Cfg { name: "P4-prodrel", base: false, rel: Rel::Prod, tree: Tree::None, lshift: 1 },
    Cfg { name: "P4-shared", base: false, rel: Rel::Shared, tree: Tree::None, lshift: 1 },
    Cfg { name: "P4-shared-tree5", base: false, rel: Rel::Shared, tree: Tree::L5, lshift: 1 },
    Cfg { name: "P4-shared-tree9", base: false, rel: Rel::Shared, tree: Tree::L9, lshift: 1 },
    Cfg { name: "P4-shared-Lpx", base: false, rel: Rel::Shared, tree: Tree::None, lshift: 3 },
];

// ------------------------------------------------------------------------------------------------ copied production pieces
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

/// Verbatim copy of production `predict`.
#[inline(always)]
fn predict3(m: i32, w: i32, ww: i32, n: i32, nn: i32, nw: i32, ne: i32, s_h: u32, s_v: u32, s45: u32, s135: u32, fa: [u64; 3]) -> (i32, [u64; 3]) {
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
    let h4 = (4 * w + ((w - ww + n - nw) >> 1) + o) as u64;
    let v4 = (4 * n + ((n - nn + w - nw) >> 1) + o) as u64;
    let d4 = (((3 * ((4 - f) * nw + f * ne)) / 4 + (w + ne - n) + o + 4 * paeth(w, n, nw) + o + 1) >> 1) as u64;
    let t = ((ph * h4 as u128 + pv * v4 as u128 + pd * d4 as u128) / d) as u64;
    ((((t + 2) >> 2) as i64 - m as i64).clamp(0, m as i64) as i32, [h4, v4, d4])
}

#[derive(Clone, Copy)]
struct Shrub3 {
    e: [i64; 3],
}
impl Shrub3 {
    #[inline(always)]
    fn factors(&self) -> [u64; 3] {
        let mn = self.e[0].min(self.e[1]).min(self.e[2]);
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

// ------------------------------------------------------------------------------------------------ pixel-only tree (from the expert-diversity study)
fn leaf5(l: usize) -> usize {
    if l < 3 {
        l
    } else {
        3 + (l - 3) / 3
    }
}
/// 9 leaves: flat / two-level / smooth / {coherent, incoherent} x 3 gradient-energy bands. Uses only W,N,NW,NE and the pixel activity sums.
#[inline(always)]
fn classify9(w: i32, n: i32, nw: i32, ne: i32, s_h: u32, s_v: u32, s_d: u32) -> usize {
    let mut v = [w, n, nw, ne];
    v.sort();
    let f = 1 + (v[1] != v[0]) as u32 + (v[2] != v[1]) as u32 + (v[3] != v[2]) as u32;
    let g = s_h + s_v;
    if f == 1 {
        0
    } else if f == 2 {
        1
    } else if g < 16 {
        2
    } else {
        let mut s = [s_h, s_v, s_d];
        s.sort();
        let inco = (s[0] * 2 > s[1]) as usize;
        let band = if g < 64 { 0 } else if g < 256 { 1 } else { 2 };
        3 + inco * 3 + band
    }
}

// ------------------------------------------------------------------------------------------------ four-expert combination
/// Optimised combination: w_i = f_i * prod_{j != i} q_j, one division. q <= 2^20 so each triple product fits u64.
#[inline(always)]
fn combine4(est: [u64; 4], q: [u64; 4], f: [u64; 4]) -> u64 {
    let w0 = f[0] as u128 * (q[1] * q[2] * q[3]) as u128;
    let w1 = f[1] as u128 * (q[0] * q[2] * q[3]) as u128;
    let w2 = f[2] as u128 * (q[0] * q[1] * q[3]) as u128;
    let w3 = f[3] as u128 * (q[0] * q[1] * q[2]) as u128;
    ((w0 * est[0] as u128 + w1 * est[1] as u128 + w2 * est[2] as u128 + w3 * est[3] as u128) / (w0 + w1 + w2 + w3)) as u64
}

/// Reference: weights as f_i * (Q / q_i) with Q = prod q (exact division), checked u128 arithmetic throughout.
/// Returns (value, weights, overflowed).
fn ref_combine4(est: [u64; 4], q: [u64; 4], f: [u64; 4]) -> (u128, [u128; 4], bool) {
    let mut ovf = false;
    let mut big: u128 = 1;
    for &x in &q {
        big = big.checked_mul(x as u128).unwrap_or_else(|| {
            ovf = true;
            1
        });
    }
    let mut w = [0u128; 4];
    let (mut num, mut den) = (0u128, 0u128);
    for i in 0..4 {
        w[i] = (f[i] as u128).checked_mul(big / q[i] as u128).unwrap_or_else(|| {
            ovf = true;
            0
        });
        match (w[i].checked_mul(est[i] as u128).and_then(|v| num.checked_add(v)), den.checked_add(w[i])) {
            (Some(n), Some(d)) => {
                num = n;
                den = d;
            }
            _ => ovf = true,
        }
    }
    (if den == 0 { 0 } else { num / den }, w, ovf)
}

#[derive(Default)]
struct Stats {
    sync: u64,
    n: u64,
    mismatch: u64,
    bound: u64,
    overflow: u64,
    clamped: u64,
    neg_est: u64,
    wshare: [f64; 4],
    gsum: [f64; 4],
    leaf_n: [u64; 9],
    mu_sum: [[f64; 4]; 9],
}

struct Pred {
    sh3: Shrub3,
    rel: [i64; 4],
    le: [[i64; 4]; 9],
    lseen: [bool; 9],
}
impl Pred {
    fn new() -> Self {
        Pred { sh3: Shrub3 { e: [0; 3] }, rel: [0; 4], le: [[0; 4]; 9], lseen: [false; 9] }
    }

    /// Global reliability g_i = max(g_min, 256 * ((Emin + c) / (E_i + c))^2), integer only.
    #[inline(always)]
    fn global_g(&self, gmin: u64) -> [u64; 4] {
        let mn = self.rel[0].min(self.rel[1]).min(self.rel[2]).min(self.rel[3]);
        let f = |e: i64| {
            let r = (((mn + C_REL) as u64) << 16) / (e + C_REL) as u64;
            ((256 * ((r * r) >> 16) + 32768) >> 16).max(gmin)
        };
        [f(self.rel[0]), f(self.rel[1]), f(self.rel[2]), f(self.rel[3])]
    }

    /// Leaf modulation mu_i (Q8, 256 = neutral): how much better/worse expert i is inside this leaf than its global record,
    /// relative to the other experts' leaf/global ratios. Squared like g, clamped to [1/4, 4].
    #[inline(always)]
    fn modulation(&self, leaf: usize) -> [u64; 4] {
        if !self.lseen[leaf] {
            return [256; 4];
        }
        let r: [u64; 4] = std::array::from_fn(|i| ((((self.le[leaf][i] + C_REL) as u64) << 8) / (self.rel[i] + C_REL) as u64).max(1));
        let mean = (r[0] + r[1] + r[2] + r[3]) / 4;
        std::array::from_fn(|i| {
            let rho = (mean << 8) / r[i];
            ((rho * rho) >> 8).clamp(64, 1024)
        })
    }

    /// Predict, resolve the sample, learn. Returns (sample, Paeth 4x error).
    #[inline(always)]
    fn px<F: FnMut(usize, u32) -> u32>(&mut self, cfg: &Cfg, chk: bool, st: &mut Stats, m: i32, nb: [i32; 8], s: [u32; 4], ep: [u32; 4], idx: usize, resolve: &mut F) -> (u32, u32) {
        let [w, ww, n, nn, nw, ne, _nnw, _nne] = nb; // NNW/NNE are in the shared neighbourhood; none of the four experts reads them (as in production)
        let [s_h, s_v, s45, s135] = s;
        if cfg.base {
            let (pr, est) = predict3(m, w, ww, n, nn, nw, ne, s_h, s_v, s45, s135, self.sh3.factors());
            if chk {
                st.sync = (st.sync ^ pr as u64).wrapping_mul(0x100000001b3);
                for e in self.sh3.e {
                    st.sync = (st.sync ^ e as u64).wrapping_mul(0x100000001b3);
                }
            }
            let xv = resolve(idx, pr as u32);
            self.sh3.update(xv as i32, est, m);
            return (xv, 0);
        }
        let mn = s45.min(s135);
        let s_d = mn + (mn >> 2);
        let o = 4 * m;
        // Independent experts, all from the same neighbourhood; none reads another's output.
        let f = 2 + (s135 >= 2 * s45) as i32 - (s45 >= 2 * s135) as i32;
        let est_i = [
            4 * w + ((w - ww + n - nw) >> 1) + o,                                   // H (production)
            4 * n + ((n - nn + w - nw) >> 1) + o,                                   // V (production)
            (3 * ((4 - f) * nw + f * ne)) / 4 + (w + ne - n) + o,                   // D' = production D without the Paeth average
            4 * paeth(w, n, nw) + o,                                                // Paeth, independent
        ];
        let est: [u64; 4] = std::array::from_fn(|i| est_i[i] as u64);
        // Local evidence: production proxies for H/V/D; Paeth from its own causal error history.
        let lp = (2 * ep[0] + 2 * ep[1] + ep[2] + ep[3]) >> cfg.lshift;
        let smax = s_h.max(s_v).max(s_d).max(lp);
        let sh = (32 - (smax | 1).leading_zeros()).saturating_sub(10);
        let sq = |s: u32| {
            let r = ((s >> sh) + 1) as u64;
            r * r
        };
        let q = [sq(s_h), sq(s_v), sq(s_d), sq(lp)];
        let (gmin, rate) = if cfg.rel == Rel::Shared { (G_MIN_SHARED, RATE_SHARED) } else { (1, RATE_PROD) };
        let g = self.global_g(gmin);
        let leaf = match cfg.tree {
            Tree::None => 0,
            Tree::L5 => leaf5(classify9(w, n, nw, ne, s_h, s_v, s_d)),
            Tree::L9 => classify9(w, n, nw, ne, s_h, s_v, s_d),
        };
        let mu = if cfg.tree == Tree::None { [256; 4] } else { self.modulation(leaf) };
        let fa: [u64; 4] = if cfg.tree == Tree::None { g } else { std::array::from_fn(|i| ((g[i] * mu[i]) >> 8).max(1)) };
        let t = combine4(est, q, fa);
        let unclamped = ((t + 2) >> 2) as i64 - m as i64;
        let pr = unclamped.clamp(0, m as i64) as i32;
        if chk {
            st.n += 1;
            let (rt, wr, ovf) = ref_combine4(est, q, fa);
            if ovf {
                st.overflow += 1;
            }
            if rt != t as u128 {
                st.mismatch += 1;
            }
            let (lo, hi) = (est.iter().min().unwrap(), est.iter().max().unwrap());
            if t < *lo || t > *hi {
                st.bound += 1;
            }
            if est_i.iter().any(|&e| e < 0) {
                st.neg_est += 1;
            }
            if unclamped != pr as i64 {
                st.clamped += 1;
            }
            let tot: f64 = wr.iter().map(|&x| x as f64).sum();
            for i in 0..4 {
                st.wshare[i] += wr[i] as f64 / tot;
                st.gsum[i] += g[i] as f64;
            }
            if cfg.tree != Tree::None {
                st.leaf_n[leaf] += 1;
                for i in 0..4 {
                    st.mu_sum[leaf][i] += mu[i] as f64;
                }
            }
            st.sync = (st.sync ^ pr as u64).wrapping_mul(0x100000001b3);
            for e in self.rel {
                st.sync = (st.sync ^ e as u64).wrapping_mul(0x100000001b3);
            }
            if cfg.tree != Tree::None {
                for e in self.le[leaf] {
                    st.sync = (st.sync ^ e as u64).wrapping_mul(0x100000001b3);
                }
            }
        }
        let xv = resolve(idx, pr as u32);
        // Every expert is scored after reconstruction, whatever its weight.
        let tgt = 4 * xv as i64 + o as i64;
        let err: [i64; 4] = std::array::from_fn(|i| (tgt - est[i] as i64).abs());
        if cfg.tree != Tree::None {
            if !self.lseen[leaf] {
                self.le[leaf] = self.rel;
                self.lseen[leaf] = true;
            }
            for i in 0..4 {
                self.le[leaf][i] += ((err[i] << 4) - self.le[leaf][i]) >> rate;
            }
        }
        for i in 0..4 {
            self.rel[i] += ((err[i] << 4) - self.rel[i]) >> rate;
        }
        (xv, err[3] as u32)
    }
}

// ------------------------------------------------------------------------------------------------ Stage 1 scan (copy of production + hooks)
struct Scan {
    w: usize,
    h: usize,
    bits: u32,
    m: u32,
    ra: Vec<u16>,
    rb: Vec<u16>,
    rc: Vec<u16>,
    da: Vec<[u16; 4]>,
    db: Vec<[u16; 4]>,
    ea: Vec<u32>, // Paeth |4x - P4| rows, index x+1
    eb: Vec<u32>,
}
impl Scan {
    fn new(w: usize, h: usize, bits: u32) -> Self {
        Scan { w, h, bits, m: (1 << bits) - 1, ra: vec![0; w + 4], rb: vec![0; w + 4], rc: vec![0; w + 4], da: vec![[0; 4]; w + 1], db: vec![[0; 4]; w + 1], ea: vec![0; w + 2], eb: vec![0; w + 2] }
    }

    #[inline(always)]
    fn scan<F: FnMut(usize, u32) -> u32>(&mut self, cfg: &Cfg, chk: bool, st: &mut Stats, mut resolve: F) {
        let (w, h, m) = (self.w, self.h, self.m as i32);
        assert!(w >= 2 && h >= 1);
        let mid = 1u32 << (self.bits - 1);
        let mut pd = Pred::new();
        let (mut cur, mut p, mut pp) = (&mut self.ra[..], &mut self.rb[..], &mut self.rc[..]);
        let (mut dcur, mut dprev) = (&mut self.da[..], &mut self.db[..]);
        let (mut ecur, mut eprev) = (&mut self.ea[..], &mut self.eb[..]);

        // Row 0: production horizontal predictor (no experts yet); its |4*(x - pred)| seeds the Paeth error history.
        let x0 = resolve(0, mid) as u16;
        cur[2] = x0;
        cur[0] = x0;
        cur[1] = x0;
        ecur[1] = 4 * (x0 as i32 - mid as i32).unsigned_abs();
        for x in 1..w {
            let wv = cur[x + 1] as i32;
            let wwv = cur[x] as i32;
            let pr = ((5 * wv - wwv + 2) >> 2).clamp(0, m);
            let v = resolve(x, pr as u32) as u16;
            cur[x + 2] = v;
            ecur[x + 1] = 4 * (v as i32 - pr).unsigned_abs();
        }
        let last = cur[w + 1];
        cur[w + 2] = last;
        cur[w + 3] = last;
        ecur[w + 1] = ecur[w];
        dcur[0] = [0; 4];
        for x in 1..w {
            let hh = (cur[x + 2] as i32 - cur[x + 1] as i32).unsigned_abs() as u16;
            dcur[x] = [hh, 0, hh, hh];
        }
        dcur[w] = [0; 4];
        pp.copy_from_slice(cur);
        std::mem::swap(&mut p, &mut cur);
        std::mem::swap(&mut dprev, &mut dcur);
        std::mem::swap(&mut eprev, &mut ecur);
        eprev[0] = eprev[1];

        for y in 1..h {
            let base = y * w;
            let n0 = p[2];
            cur[0] = n0;
            cur[1] = n0;
            ecur[0] = eprev[1];
            let (mut h_c, mut v_c, mut a_c);
            {
                let n = n0 as i32;
                let (nn, ne) = (pp[2] as i32, p[3] as i32);
                let (d0, d1) = (dprev[0], dprev[1]);
                let s = [d0[0] as u32 + d1[0] as u32, d0[1] as u32 + d1[1] as u32, d0[2] as u32, d1[3] as u32 + d0[3] as u32];
                let ep = [ecur[0], eprev[1], eprev[0], eprev[2]];
                let (xv, pe) = pd.px(cfg, chk, st, m, [n, n, n, nn, n, ne, pp[2] as i32, pp[3] as i32], s, ep, base, &mut resolve);
                ecur[1] = pe;
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
                let s = [h_c + d0[0] as u32 + d1[0] as u32, v_c + d0[1] as u32 + d1[1] as u32, a_c + d0[2] as u32, d1[3] as u32 + d0[3] as u32];
                let ep = [ecur[x], eprev[x + 1], eprev[x], eprev[x + 2]];
                let (xv, pe) = pd.px(cfg, chk, st, m, [wv, wwv, n, nn, nw, ne, pp[i - 1] as i32, pp[i + 1] as i32], s, ep, base + x, &mut resolve);
                ecur[x + 1] = pe;
                let xv = xv as i32;
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
            let last = cur[w + 1];
            cur[w + 2] = last;
            cur[w + 3] = last;
            dcur[w] = [0, v_c as u16, 0, v_c as u16];
            ecur[w + 1] = ecur[w];
            std::mem::swap(&mut pp, &mut p);
            std::mem::swap(&mut p, &mut cur);
            std::mem::swap(&mut dprev, &mut dcur);
            std::mem::swap(&mut eprev, &mut ecur);
            eprev[0] = eprev[1];
        }
    }
}

// ------------------------------------------------------------------------------------------------ plane codec (production Stage 3 + Stage 4)
fn enc_scan(src: &[u16], w: usize, h: usize, bits: u32, cfg: &Cfg, chk: bool, st: &mut Stats, res: &mut [i32], pred: Option<&mut [u16]>) {
    let mut sc = Scan::new(w, h, bits);
    match pred {
        Some(pr) => sc.scan(cfg, chk, st, |i, p| {
            res[i] = src[i] as i32 - p as i32;
            pr[i] = p as u16;
            src[i] as u32
        }),
        None => sc.scan(cfg, chk, st, |i, p| {
            res[i] = src[i] as i32 - p as i32;
            src[i] as u32
        }),
    }
}

fn enc_plane(src: &[u16], w: usize, h: usize, bits: u32, cfg: &Cfg, enc: &mut Stage4Enc, res: &mut [i32]) -> Vec<u8> {
    let mut st = Stats::default();
    enc_scan(src, w, h, bits, cfg, false, &mut st, res, None);
    let mut s3 = Stage3::with_profile(w, h, bits, Stage3Profile::Bc255S11Direct).unwrap();
    enc.reset();
    s3.encode_plane(res, enc).unwrap();
    enc.finish().to_vec()
}

fn dec_plane(stream: &[u8], out: &mut [u16], w: usize, h: usize, bits: u32, cfg: &Cfg, chk: bool, st: &mut Stats, res: &mut [i32]) -> bool {
    let mut dec = Stage4Dec::new(stream);
    let mut s3 = Stage3::with_profile(w, h, bits, Stage3Profile::Bc255S11Direct).unwrap();
    s3.decode_plane(res, &mut dec).unwrap();
    let mut sc = Scan::new(w, h, bits);
    let m = (1i32 << bits) - 1;
    let mut bad = false;
    sc.scan(cfg, chk, st, |i, p| {
        let v = p as i32 + res[i];
        bad |= (v as u32) > m as u32;
        let v = v.clamp(0, m);
        out[i] = v as u16;
        v as u32
    });
    !bad
}

// ------------------------------------------------------------------------------------------------ harness
fn center_crop(img: &Canonical, max: u32) -> Canonical {
    let (w, h) = (img.spec.width, img.spec.height);
    if w <= max && h <= max {
        return img.clone();
    }
    let (cw, ch) = (w.min(max), h.min(max));
    let (x0, y0) = ((w - cw) / 2, (h - ch) / 2);
    let row = img.spec.layout.channels() * img.spec.bytes_per_sample();
    let mut data = Vec::new();
    for y in y0..y0 + ch {
        let s = (y as usize * w as usize + x0 as usize) * row;
        data.extend_from_slice(&img.data[s..s + cw as usize * row]);
    }
    Canonical::new(Spec { width: cw, height: ch, ..img.spec }, data).unwrap()
}

fn split_planes(img: &Canonical, ycocg: bool) -> Vec<Vec<u16>> {
    let ch = img.spec.layout.channels();
    let n = img.spec.width as usize * img.spec.height as usize;
    let mut planes = vec![vec![0u16; n]; ch];
    for (i, px) in img.data.chunks_exact(ch).enumerate() {
        for (k, &v) in px.iter().enumerate() {
            planes[k][i] = v as u16;
        }
    }
    if ycocg {
        assert!(ellie::ycocg_r_forward_planes(&mut planes[..3]));
    }
    planes
}

fn pack(mut planes: Vec<Vec<u16>>, ycocg: bool, n: usize) -> Option<Vec<u8>> {
    if ycocg && !ellie::ycocg_r_inverse_planes(&mut planes[..3]) {
        return None;
    }
    let ch = planes.len();
    let mut data = vec![0u8; n * ch];
    for (k, pl) in planes.iter().enumerate() {
        for (i, &v) in pl.iter().enumerate() {
            data[i * ch + k] = v as u8;
        }
    }
    Some(data)
}

fn timed<T>(reps: usize, mut f: impl FnMut() -> T) -> (T, f64, f64) {
    let mut ts = vec![];
    let mut keep = None;
    for _ in 0..reps {
        let t = Instant::now();
        let r = f();
        ts.push(t.elapsed().as_secs_f64() * 1e3);
        keep = Some(r);
    }
    ts.sort_by(|a, b| a.partial_cmp(b).unwrap());
    (keep.unwrap(), ts[0], ts[ts.len() / 2])
}

struct Row {
    image: String,
    cfg: &'static str,
    stream: usize,
    total: usize,
    raw: usize,
    enc_best: f64,
    enc_med: f64,
    dec_best: f64,
    dec_med: f64,
    scan_best: f64,
    exact: bool,
    det: bool,
    sync: bool,
    ref_ok: bool,
    stats: Stats,
}

fn main() {
    if cfg!(debug_assertions) {
        eprintln!("use --release");
        std::process::exit(2);
    }
    let a: Vec<String> = std::env::args().collect();
    let arg = |k: &str| a.iter().position(|x| x == k).and_then(|i| a.get(i + 1)).cloned();
    let out = PathBuf::from(arg("--out").expect("--out <new dir>"));
    let reps: usize = arg("--reps").map(|s| s.parse().unwrap()).unwrap_or(5);
    if out.exists() {
        eprintln!("refusing to reuse existing output dir {}", out.display());
        std::process::exit(2);
    }
    std::fs::create_dir_all(&out).unwrap();
    let mut rows: Vec<Row> = vec![];
    let mut prod_rows: Vec<(String, usize, usize, f64, f64)> = vec![]; // name, prod bytes, raw, enc single-repr ms, dec ms
    let mut gate_notes = String::new();
    let mut all_ok = true;

    for f in ["portrait_0.jpg", "screen_0.png", "night_0.jpg"] {
        let name = f.split('.').next().unwrap().to_string();
        let img = center_crop(&Canonical::load(&Path::new("data").join(f)).unwrap(), CROP);
        let (w, h) = (img.spec.width as usize, img.spec.height as usize);
        let prod = pkon::encode_standard_with(&img, EllieRepr::Auto).unwrap();
        assert_eq!(pkon::encode_standard_with(&img, EllieRepr::Auto).unwrap(), prod, "production nondeterministic");
        let hdr = pkon::parse_header(&prod).unwrap();
        let CodecParams::Ellie(ep) = &hdr.params else { panic!("not ELLIE") };
        let ycocg = ep.representation == ELLIE_REPR_YCOCG_R;
        let single = if ycocg { EllieRepr::YCoCgR } else { EllieRepr::Untransformed };
        let (_, p_enc, _) = timed(reps, || pkon::encode_standard_with(&img, single).unwrap());
        let (_, p_dec, _) = timed(reps, || pkon::decode(&prod).unwrap());
        let planes = split_planes(&img, ycocg);
        let bits: Vec<u32> = ep.planes.iter().map(|p| p.0 as u32).collect();
        let payload = &prod[prod.len() - hdr.payload_len as usize..];
        let overhead = prod.len() - payload.len();
        let raw = img.data.len();
        eprintln!("[{name}] production {} B (overhead {overhead}), ycocg={ycocg}, planes={}", prod.len(), planes.len());
        prod_rows.push((name.clone(), prod.len(), raw, p_enc, p_dec));

        // ---- Reproduction gate: copied scan vs live Stage1 prediction + residual planes, then byte identity.
        let mut res = vec![0i32; w * h];
        for (k, pl) in planes.iter().enumerate() {
            let mut s1 = Stage1::new(w, h, bits[k]).unwrap();
            let (mut r0, mut p0) = (vec![0i32; w * h], vec![0u16; w * h]);
            s1.encode_with_pred(pl, &mut r0, &mut p0).unwrap();
            let mut p1 = vec![0u16; w * h];
            let mut st = Stats::default();
            enc_scan(pl, w, h, bits[k], &CFGS[0], false, &mut st, &mut res, Some(&mut p1));
            assert!(p1 == p0 && res == r0, "{name} plane {k}: copied production scan differs from live Stage1");
            let _ = writeln!(gate_notes, "{name} plane {k}: prediction plane and residuals identical to live Stage1 ({} px)", w * h);
        }
        let mut enc = Stage4Enc::with_capacity(w * h);

        for cfg in CFGS.iter() {
            let (streams, enc_best, enc_med) = timed(reps, || planes.iter().zip(&bits).map(|(p, &b)| enc_plane(p, w, h, b, cfg, &mut enc, &mut res)).collect::<Vec<_>>());
            let streams2: Vec<Vec<u8>> = planes.iter().zip(&bits).map(|(p, &b)| enc_plane(p, w, h, b, cfg, &mut enc, &mut res)).collect();
            let det = streams == streams2;
            let ((rec, ok), dec_best, dec_med) = timed(reps, || {
                let mut o = vec![vec![0u16; w * h]; planes.len()];
                let mut ok = true;
                let mut st = Stats::default();
                for ((s, op), &b) in streams.iter().zip(o.iter_mut()).zip(&bits) {
                    ok &= dec_plane(s, op, w, h, b, cfg, false, &mut st, &mut res);
                }
                (o, ok)
            });
            let plane_exact = rec == planes;
            let rgb_exact = pack(rec, ycocg, w * h).as_deref() == Some(&img.data[..]);
            let exact = ok && plane_exact && rgb_exact;
            // Scan-only (prediction/tree/reliability cost, no entropy coding).
            let (_, scan_best, _) = timed(reps, || {
                let mut st = Stats::default();
                for (p, &b) in planes.iter().zip(&bits) {
                    enc_scan(p, w, h, b, cfg, false, &mut st, &mut res, None);
                }
            });
            // Verification pass: encoder and decoder predictor state in lockstep, u128 reference, bounds.
            let (mut se, mut sd) = (Stats::default(), Stats::default());
            let mut sync = true;
            for (k, p) in planes.iter().enumerate() {
                let (mut a, mut b) = (Stats::default(), Stats::default());
                enc_scan(p, w, h, bits[k], cfg, true, &mut a, &mut res, None);
                let mut o = vec![0u16; w * h];
                dec_plane(&streams[k], &mut o, w, h, bits[k], cfg, true, &mut b, &mut res);
                sync &= a.sync == b.sync && a.n == b.n;
                for (x, y) in [(&mut se, &a), (&mut sd, &b)] {
                    x.n += y.n;
                    x.mismatch += y.mismatch;
                    x.bound += y.bound;
                    x.overflow += y.overflow;
                    x.clamped += y.clamped;
                    x.neg_est += y.neg_est;
                    for i in 0..4 {
                        x.wshare[i] += y.wshare[i];
                        x.gsum[i] += y.gsum[i];
                    }
                    for l in 0..9 {
                        x.leaf_n[l] += y.leaf_n[l];
                        for i in 0..4 {
                            x.mu_sum[l][i] += y.mu_sum[l][i];
                        }
                    }
                }
            }
            let ref_ok = se.mismatch + sd.mismatch + se.bound + sd.bound + se.overflow + sd.overflow + se.neg_est == 0;
            let stream: usize = streams.iter().map(|s| s.len()).sum();
            let total = overhead + stream + (!cfg.base) as usize; // +1 mode byte whenever the swarm is in use
            if cfg.base {
                assert_eq!(streams.concat(), payload, "{name}: baseline streams differ from production payload");
                assert_eq!(total, prod.len());
            }
            eprintln!("  {:<16} {:>8} B  enc {:>6.0}/{:>6.0} ms dec {:>6.0}/{:>6.0} ms scan {:>5.0} ms exact={exact} det={det} sync={sync} ref={ref_ok}", cfg.name, total, enc_best, enc_med, dec_best, dec_med, scan_best);
            all_ok &= exact && det && sync && ref_ok;
            rows.push(Row { image: name.clone(), cfg: cfg.name, stream, total, raw, enc_best, enc_med, dec_best, dec_med, scan_best, exact, det, sync, ref_ok, stats: se });
        }
    }

    // setup time
    let (_, setup_best, _) = timed(200, || {
        for _ in 0..50 {
            std::hint::black_box((Scan::new(CROP as usize, CROP as usize, 8), Pred::new()));
        }
    });
    let setup_us = setup_best * 1e3 / 50.0;

    // ---- reports (rejected rows are marked, not tabulated as results)
    let prod_total: usize = prod_rows.iter().map(|r| r.1).sum();
    let raw_total: usize = prod_rows.iter().map(|r| r.2).sum();
    let mut csv = String::from("image,config,stream_bytes,total_bytes,ratio_raw_over_file,enc_best_ms,enc_median_ms,dec_best_ms,dec_median_ms,scan_only_best_ms,roundtrip_exact,deterministic,state_sync,u128_ref_bounds_ok\n");
    for r in &rows {
        let _ = writeln!(csv, "{},{},{},{},{:.4},{:.1},{:.1},{:.1},{:.1},{:.1},{},{},{},{}", r.image, r.cfg, r.stream, r.total, r.raw as f64 / r.total as f64, r.enc_best, r.enc_med, r.dec_best, r.dec_med, r.scan_best, r.exact, r.det, r.sync, r.ref_ok);
    }
    std::fs::write(out.join("per_image.csv"), csv).unwrap();

    let mut md = String::new();
    let _ = writeln!(md, "# ELLIE four-expert swarm: canary results\n\nRelease build, 1024-px centre crops, representation chosen by production Auto. Sizes = production container overhead + real Stage 3/4 streams (+1 mode byte for every swarm config). Production total: {prod_total} B ({}). Raw total {raw_total} B. Timings: best / median of {reps} runs, single thread, in-process.\n", prod_rows.iter().map(|b| format!("{} {}", b.0, b.1)).collect::<Vec<_>>().join(", "));
    let _ = writeln!(md, "## Reproduction gate\n\n```\n{gate_notes}```\nprod-copy streams byte-identical to the production payload and total file size equal to production on all three images (asserted).\n");
    let _ = writeln!(md, "## Bytes\n\n| config | total B | vs prod (B) | vs prod % | portrait B (%) | screen B (%) | night B (%) | ratio raw/file | round-trip | deterministic |\n|---|---|---|---|---|---|---|---|---|---|");
    for c in CFGS.iter() {
        let rs: Vec<&Row> = rows.iter().filter(|r| r.cfg == c.name).collect();
        let tot: usize = rs.iter().map(|r| r.total).sum();
        let per: Vec<String> = rs.iter().zip(&prod_rows).map(|(r, b)| format!("{} ({:+.2}%)", r.total, (r.total as f64 / b.1 as f64 - 1.0) * 100.0)).collect();
        let ok = rs.iter().all(|r| r.exact && r.sync && r.ref_ok);
        let _ = writeln!(md, "| {} | {} | {:+} | {:+.3}% | {} | {} | {} | {:.4} | {} | {} |", c.name, tot, tot as i64 - prod_total as i64, (tot as f64 / prod_total as f64 - 1.0) * 100.0, per[0], per[1], per[2], raw_total as f64 / tot as f64, if ok { "exact" } else { "FAILED (result rejected)" }, rs.iter().all(|r| r.det));
    }
    let _ = writeln!(md, "\n## Timing (ms, sum over the 3 images; best / median)\n\nProduction `pkon` single-representation encode {:.0} ms, decode {:.0} ms (best). Harness configs use safe indexing and an unfused Stage1->Stage3 split, so compare within the table (prod-copy is the reference). scan-only = prediction/reliability/tree work with no entropy coding.\n\n| config | encode | decode | scan-only | enc vs prod-copy | dec vs prod-copy |\n|---|---|---|---|---|---|", prod_rows.iter().map(|r| r.3).sum::<f64>(), prod_rows.iter().map(|r| r.4).sum::<f64>());
    let sum = |c: &str, f: fn(&Row) -> f64| rows.iter().filter(|r| r.cfg == c).map(f).sum::<f64>();
    let (be, bd) = (sum("prod-copy", |r| r.enc_best), sum("prod-copy", |r| r.dec_best));
    for c in CFGS.iter() {
        let (e, d, s) = (sum(c.name, |r| r.enc_best), sum(c.name, |r| r.dec_best), sum(c.name, |r| r.scan_best));
        let _ = writeln!(md, "| {} | {:.0} / {:.0} | {:.0} / {:.0} | {:.0} | {:+.1}% | {:+.1}% |", c.name, e, sum(c.name, |r| r.enc_med), d, sum(c.name, |r| r.dec_med), s, (e / be - 1.0) * 100.0, (d / bd - 1.0) * 100.0);
    }
    let _ = writeln!(md, "\nTree/state setup (Scan buffers + Pred state for a 1024-wide plane): {setup_us:.1} us. The tree itself has no construction phase: leaves are classified inline per pixel from causal pixels, no stored thresholds.\n");
    let _ = writeln!(md, "## Verification (encoder + decoder verification pass, all planes)\n\n| image | config | pixels | enc/dec state-sync | u128 mismatches | bound violations | overflow | negative estimates | clamp hits |\n|---|---|---|---|---|---|---|---|---|");
    for r in rows.iter().filter(|r| r.cfg != "prod-copy") {
        let _ = writeln!(md, "| {} | {} | {} | {} | {} | {} | {} | {} | {} |", r.image, r.cfg, r.stats.n, r.sync, r.stats.mismatch, r.stats.bound, r.stats.overflow, r.stats.neg_est, r.stats.clamped);
    }
    let _ = writeln!(md, "\n## Shared reliability behaviour (mean per-pixel normalised weight share / mean global g_i)\n\n| image | config | H | V | D' | Paeth | g: H / V / D' / Paeth |\n|---|---|---|---|---|---|---|");
    for r in rows.iter().filter(|r| r.cfg != "prod-copy" && r.stats.n > 0) {
        let n = r.stats.n as f64;
        let _ = writeln!(md, "| {} | {} | {:.1}% | {:.1}% | {:.1}% | {:.1}% | {:.0} / {:.0} / {:.0} / {:.0} |", r.image, r.cfg, r.stats.wshare[0] / n * 100.0, r.stats.wshare[1] / n * 100.0, r.stats.wshare[2] / n * 100.0, r.stats.wshare[3] / n * 100.0, r.stats.gsum[0] / n, r.stats.gsum[1] / n, r.stats.gsum[2] / n, r.stats.gsum[3] / n);
    }
    for tname in ["P4-shared-tree5", "P4-shared-tree9"] {
        let _ = writeln!(md, "\n## Tree modulation, {tname}: leaf share and mean mu (Q8, 256 = neutral) per expert, all images pooled\n\n| leaf | pixels % | mu H | mu V | mu D' | mu Paeth |\n|---|---|---|---|---|---|");
        let mut ln = [0u64; 9];
        let mut ms = [[0f64; 4]; 9];
        for r in rows.iter().filter(|r| r.cfg == tname) {
            for l in 0..9 {
                ln[l] += r.stats.leaf_n[l];
                for i in 0..4 {
                    ms[l][i] += r.stats.mu_sum[l][i];
                }
            }
        }
        let tot: u64 = ln.iter().sum();
        for l in 0..9 {
            if ln[l] > 0 {
                let c = ln[l] as f64;
                let _ = writeln!(md, "| {l} | {:.1}% | {:.0} | {:.0} | {:.0} | {:.0} |", c / tot as f64 * 100.0, ms[l][0] / c, ms[l][1] / c, ms[l][2] / c, ms[l][3] / c);
            }
        }
    }
    let _ = writeln!(md, "\nAll round-trip, determinism, state-sync, u128-reference and bounds checks passed: {all_ok}.");
    std::fs::write(out.join("summary.md"), &md).unwrap();
    println!("{md}");
    if !all_ok {
        std::process::exit(1);
    }
}
