//! ELLIE predictor-swarm, step 1: offline expert-diversity analysis. Diagnostic only; no codec, no entropy coder.
//!
//! Five independent experts (H, V, D', Paeth, Smooth) are evaluated on the ORIGINAL planes of the three-image canary
//! (1024-px centre crops, representation chosen by production `Auto`). Because every expert reads only the causal
//! neighbourhood and the decoder reconstructs the original pixels exactly, evaluating on the original is exact.
//! The Stage 1 scan is copied here (it is private) so the neighbourhood and activity sums are available; every
//! pixel's copied `predict` result is asserted equal to the live `Stage1::encode_with_pred` prediction plane.
//!
//! Errors d_i = 4x - clamp(P4_i, 0, 4M). Bit proxy per pixel: log2(1 + |r|/4), r in 4x units.
//! Analysed pixels: rows >= 1 and columns >= 1 (the general loop of Stage 1); row 0 and column 0 are excluded.
//!
//!   cargo run --release --bin exp_ellie_expert_diversity -- --out <dir>
use nanoscope::canon::{Canonical, Spec};
use nanoscope::ellie::{self, Stage1};
use nanoscope::pkon::{self, CodecParams, EllieRepr, ELLIE_REPR_YCOCG_R};
use std::fmt::Write as _;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

const K: usize = 5;
const NAMES: [&str; K] = ["H", "V", "D'", "Paeth", "Smooth"];
const LN5: [&str; 5] = ["flat(F=1)", "two-level(F=2)", "smooth", "cont-coherent", "cont-incoherent"];
const FULL: usize = (1 << K) - 1;
const HVD: usize = 0b00111;
const BLK_ROWS: usize = 64;
const CAUCHY_C2: f64 = 16.0;
const IRLS_IT: usize = 4;
const BOOT: usize = 200;

// ------------------------------------------------------------------------------ copied Stage 1 (private in the crate)
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
    let h4 = (4 * w + ((w - ww + n - nw) >> 1) + o) as u64;
    let v4 = (4 * n + ((n - nn + w - nw) >> 1) + o) as u64;
    let d4 = (((3 * ((4 - f) * nw + f * ne)) / 4 + (w + ne - n) + o + 4 * paeth(w, n, nw) + o + 1) >> 1) as u64;
    let t = ((ph * h4 as u128 + pv * v4 as u128 + pd * d4 as u128) / d) as u64;
    ((((t + 2) >> 2) as i64 - m as i64).clamp(0, m as i64) as i32, [h4, v4, d4])
}
#[derive(Clone, Copy)]
struct Shrub {
    e: [i64; 3],
}
impl Shrub {
    fn factors(&self) -> [u64; 3] {
        let mn = self.e[0].min(self.e[1]).min(self.e[2]);
        let f = |e: i64| {
            let r = (((mn + 16) as u64) << 16) / (e + 16) as u64;
            ((256 * ((r * r) >> 16) + 32768) >> 16).max(1)
        };
        [f(self.e[0]), f(self.e[1]), f(self.e[2])]
    }
    fn update(&mut self, x: i32, est4: [u64; 3], m: i32) {
        let t = 4 * x as i64 + 4 * m as i64;
        for k in 0..3 {
            let err = (t - est4[k] as i64).abs() << 4;
            self.e[k] += (err - self.e[k]) >> 3;
        }
    }
}
/// Everything the production predictor saw at one general-loop pixel, plus its prediction.
struct Px {
    i: usize,
    x: usize,
    y: usize,
    m: i32,
    w: i32,
    ww: i32,
    n: i32,
    nn: i32,
    nw: i32,
    ne: i32,
    s_h: u32,
    s_v: u32,
    s45: u32,
    s135: u32,
    pr: i32,
    est: [u64; 3],
    xv: i32,
}
fn scan<G: FnMut(&Px)>(src: &[u16], w: usize, h: usize, bits: u32, mut cb: G) {
    let m = ((1u32 << bits) - 1) as i32;
    let mid = 1u32 << (bits - 1);
    let mut ra = vec![0u16; w + 4];
    let mut rb = vec![0u16; w + 4];
    let mut rc = vec![0u16; w + 4];
    let mut da = vec![[0u16; 4]; w + 1];
    let mut db = vec![[0u16; 4]; w + 1];
    let mut shrub = Shrub { e: [0; 3] };
    let mut cur: &mut [u16] = &mut ra[..];
    let mut p: &mut [u16] = &mut rb[..];
    let mut pp: &mut [u16] = &mut rc[..];
    let mut dcur: &mut [[u16; 4]] = &mut da[..];
    let mut dprev: &mut [[u16; 4]] = &mut db[..];
    let _ = mid;
    let x0 = src[0];
    cur[2] = x0;
    cur[0] = x0;
    cur[1] = x0;
    for x in 1..w {
        cur[x + 2] = src[x];
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
    for y in 1..h {
        let base = y * w;
        let n0 = p[2];
        cur[0] = n0;
        cur[1] = n0;
        let (mut h_c, mut v_c, mut a_c);
        {
            let n = n0 as i32;
            let (nn, ne) = (pp[2] as i32, p[3] as i32);
            let (d0, d1) = (dprev[0], dprev[1]);
            let s_h = d0[0] as u32 + d1[0] as u32;
            let s_v = d0[1] as u32 + d1[1] as u32;
            let s45 = d0[2] as u32;
            let s135 = d1[3] as u32 + d0[3] as u32;
            let (_pr, est) = predict(m, n, n, n, nn, n, ne, s_h, s_v, s45, s135, shrub.factors());
            let xv = src[base] as u32;
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
            let xv = src[base + x] as i32;
            cb(&Px { i: base + x, x, y, m, w: wv, ww: wwv, n, nn, nw, ne, s_h, s_v, s45, s135, pr, est, xv });
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
        let last = cur[w + 1];
        cur[w + 2] = last;
        cur[w + 3] = last;
        dcur[w] = [0, v_c as u16, 0, v_c as u16];
        std::mem::swap(&mut pp, &mut p);
        std::mem::swap(&mut p, &mut cur);
        std::mem::swap(&mut dprev, &mut dcur);
    }
}

// ------------------------------------------------------------------------------ pixel-only tree (9 leaves; leaf5 merges the bands)
fn leaf5(l: u8) -> usize {
    if l < 3 {
        l as usize
    } else {
        3 + ((l - 3) / 3) as usize
    }
}
fn classify(w: i32, n: i32, nw: i32, ne: i32, s_h: u32, s_v: u32, s_d: u32) -> u8 {
    let mut v = [w, n, nw, ne];
    v.sort();
    let mut f = 1;
    for i in 1..4 {
        if v[i] != v[i - 1] {
            f += 1;
        }
    }
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
        let inco = if s[0] * 2 <= s[1] { 0 } else { 1 };
        let band = if g < 64 { 0 } else if g < 256 { 1 } else { 2 };
        3 + inco * 3 + band
    }
}

// ------------------------------------------------------------------------------ data
#[derive(Default)]
struct Data {
    d: Vec<[i16; K]>,
    dprod: Vec<i16>,
    dq4: Vec<i16>, // Smooth at 4x precision (precision check only, not in the roster)
    leaf: Vec<u8>,
    img: Vec<u8>,
    par: Vec<u8>,
    blk: Vec<u16>,
    blk_img: Vec<u8>,
}

#[derive(Clone, Default)]
struct Corr {
    n: f64,
    sa: f64,
    sb: f64,
    saa: f64,
    sbb: f64,
    sab: f64,
}
impl Corr {
    fn add(&mut self, a: f64, b: f64) {
        self.n += 1.0;
        self.sa += a;
        self.sb += b;
        self.saa += a * a;
        self.sbb += b * b;
        self.sab += a * b;
    }
    fn merge(&mut self, o: &Corr) {
        self.n += o.n;
        self.sa += o.sa;
        self.sb += o.sb;
        self.saa += o.saa;
        self.sbb += o.sbb;
        self.sab += o.sab;
    }
    fn r(&self) -> f64 {
        let n = self.n;
        let cov = self.sab - self.sa * self.sb / n;
        let (va, vb) = (self.saa - self.sa * self.sa / n, self.sbb - self.sb * self.sb / n);
        cov / (va * vb).sqrt()
    }
}
const HB: usize = 4096;
#[derive(Clone)]
struct Calib {
    n: f64,
    l_sum: [f64; K],
    s_sum: [f64; 3],
    l_hist: Vec<Vec<u32>>,
    s_hist: Vec<Vec<u32>>,
    cle: Vec<Corr>, // log L_i vs log own error (5)
    cse: Vec<Corr>, // log S_i vs log own error (3: H,V,D)
    cls: Vec<Corr>, // log L_i vs log S_i (3)
    cge: Vec<Corr>, // log (S_H+S_V) vs log own error, for Paeth, Smooth (2)
}
impl Calib {
    fn new() -> Self {
        Calib {
            n: 0.0,
            l_sum: [0.0; K],
            s_sum: [0.0; 3],
            l_hist: vec![vec![0; HB]; K],
            s_hist: vec![vec![0; HB]; 3],
            cle: vec![Corr::default(); K],
            cse: vec![Corr::default(); 3],
            cls: vec![Corr::default(); 3],
            cge: vec![Corr::default(); 2],
        }
    }
    fn merge(&mut self, o: &Calib) {
        self.n += o.n;
        for i in 0..K {
            self.l_sum[i] += o.l_sum[i];
            for b in 0..HB {
                self.l_hist[i][b] += o.l_hist[i][b];
            }
            self.cle[i].merge(&o.cle[i]);
        }
        for i in 0..3 {
            self.s_sum[i] += o.s_sum[i];
            for b in 0..HB {
                self.s_hist[i][b] += o.s_hist[i][b];
            }
            self.cse[i].merge(&o.cse[i]);
            self.cls[i].merge(&o.cls[i]);
        }
        for i in 0..2 {
            self.cge[i].merge(&o.cge[i]);
        }
    }
}
fn hist_median(h: &[u32]) -> usize {
    let tot: u64 = h.iter().map(|&c| c as u64).sum();
    let mut acc = 0u64;
    for (i, &c) in h.iter().enumerate() {
        acc += c as u64;
        if acc * 2 >= tot {
            return i;
        }
    }
    0
}
fn lg(x: f64) -> f64 {
    (1.0 + x).ln()
}

fn run_plane(src: &[u16], w: usize, h: usize, bits: u32, img: u8, data: &mut Data, calib: &mut Calib, verified: &mut u64) {
    let n = w * h;
    let mut pred = vec![0u16; n];
    let mut res = vec![0i32; n];
    Stage1::new(w, h, bits).unwrap().encode_with_pred(src, &mut res, &mut pred).unwrap();
    let mut dp = vec![[0i16; K]; n];
    let mut sv = vec![[0u32; 3]; n];
    let blk_base = data.blk_img.len();
    for _ in 0..(h + BLK_ROWS - 1) / BLK_ROWS {
        data.blk_img.push(img);
    }
    scan(src, w, h, bits, |p| {
        assert_eq!(p.pr, pred[p.i] as i32, "copied Stage 1 differs from production at {}", p.i);
        *verified += 1;
        let m = p.m;
        let o = 4 * m;
        let f = 2 + (p.s135 >= 2 * p.s45) as i32 - (p.s45 >= 2 * p.s135) as i32;
        let h4 = 4 * p.w + ((p.w - p.ww + p.n - p.nw) >> 1);
        let v4 = 4 * p.n + ((p.n - p.nn + p.w - p.nw) >> 1);
        let dd = (3 * ((4 - f) * p.nw + f * p.ne)) / 4 + (p.w + p.ne - p.n);
        let pa = 4 * paeth(p.w, p.n, p.nw);
        // Production H/V/D estimates must be recoverable from the independent experts.
        assert_eq!(p.est[0] as i64, (h4 + o) as i64);
        assert_eq!(p.est[1] as i64, (v4 + o) as i64);
        assert_eq!(p.est[2] as i64, ((dd + o + pa + o + 1) >> 1) as i64);
        let sm = ((3 * p.w + 3 * p.n + p.nw + p.ne + 4) >> 3) * 4; // authorised formula: integer-pixel prediction, 4x scale
        let sm4 = (3 * p.w + 3 * p.n + p.nw + p.ne + 1) >> 1; // precision check: same taps at 4x precision
        let x4 = 4 * p.xv;
        let c = |v: i32| (x4 - v.clamp(0, o)) as i16;
        let d = [c(h4), c(v4), c(dd), c(pa), c(sm)];
        let mn = p.s45.min(p.s135);
        let s_d = mn + (mn >> 2);
        dp[p.i] = d;
        sv[p.i] = [p.s_h, p.s_v, s_d];
        data.d.push(d);
        data.dprod.push((x4 - 4 * p.pr) as i16);
        data.dq4.push(c(sm4));
        data.leaf.push(classify(p.w, p.n, p.nw, p.ne, p.s_h, p.s_v, s_d));
        data.img.push(img);
        data.par.push((p.y & 1) as u8);
        data.blk.push((blk_base + p.y / BLK_ROWS) as u16);
        let _ = p.x;
    });
    // Local-evidence calibration on pixels whose four neighbours are all analysed pixels.
    let e = |i: usize, k: usize| ((dp[i][k] as i32).abs() + 2) >> 2;
    for y in 2..h {
        for x in 2..w - 1 {
            let i = y * w + x;
            calib.n += 1.0;
            let ss = [sv[i][0], sv[i][1], sv[i][2]];
            for k in 0..K {
                let l = (2 * e(i - 1, k) + 2 * e(i - w, k) + e(i - w - 1, k) + e(i - w + 1, k)) >> 1;
                let err = dp[i][k].abs() as f64 / 4.0;
                calib.l_sum[k] += l as f64;
                calib.l_hist[k][(l as usize).min(HB - 1)] += 1;
                calib.cle[k].add(lg(l as f64), lg(err));
                if k < 3 {
                    calib.cls[k].add(lg(l as f64), lg(ss[k] as f64));
                    calib.cse[k].add(lg(ss[k] as f64), lg(err));
                } else {
                    calib.cge[k - 3].add(lg((ss[0] + ss[1]) as f64), lg(err));
                }
            }
            for k in 0..3 {
                calib.s_sum[k] += ss[k] as f64;
                calib.s_hist[k][(ss[k] as usize).min(HB - 1)] += 1;
            }
        }
    }
}

// ------------------------------------------------------------------------------ convex-combination fitting (diagnostic bound)
fn solve_small(a: &mut [[f64; 6]; 5], k: usize) -> bool {
    // Gauss-Jordan on k x (k+1) augmented system.
    for c in 0..k {
        let mut piv = c;
        for r in c + 1..k {
            if a[r][c].abs() > a[piv][c].abs() {
                piv = r;
            }
        }
        if a[piv][c].abs() < 1e-18 {
            return false;
        }
        a.swap(c, piv);
        let d = a[c][c];
        for j in c..=k {
            a[c][j] /= d;
        }
        for r in 0..k {
            if r != c {
                let f = a[r][c];
                if f != 0.0 {
                    for j in c..=k {
                        a[r][j] -= f * a[c][j];
                    }
                }
            }
        }
    }
    true
}
/// min w'Gw, w >= 0, sum w = 1, over the experts in `idx`; exact by support enumeration (k <= 5).
fn solve_qp(g: &[[f64; K]; K], idx: &[usize]) -> [f64; K] {
    let k = idx.len();
    let tr: f64 = idx.iter().map(|&i| g[i][i]).sum::<f64>() / k as f64;
    let ridge = 1e-9 * tr + 1e-12;
    let mut best = (f64::INFINITY, [0.0; K]);
    for sup in 1u32..(1 << k) {
        let sel: Vec<usize> = (0..k).filter(|b| sup >> b & 1 == 1).map(|b| idx[b]).collect();
        let s = sel.len();
        let mut a = [[0.0f64; 6]; 5];
        for (r, &ir) in sel.iter().enumerate() {
            for (c, &ic) in sel.iter().enumerate() {
                a[r][c] = g[ir.min(ic)][ir.max(ic)] + if r == c { ridge } else { 0.0 };
            }
            a[r][s] = 1.0;
        }
        if !solve_small(&mut a, s) {
            continue;
        }
        let z: Vec<f64> = (0..s).map(|r| a[r][s]).collect();
        let sz: f64 = z.iter().sum();
        if sz <= 0.0 {
            continue;
        }
        let w: Vec<f64> = z.iter().map(|v| v / sz).collect();
        if w.iter().any(|&v| v < -1e-12) {
            continue;
        }
        let mut obj = 0.0;
        for (r, &ir) in sel.iter().enumerate() {
            for (c, &ic) in sel.iter().enumerate() {
                obj += w[r] * w[c] * g[ir.min(ic)][ir.max(ic)];
            }
        }
        if obj < best.0 {
            let mut full = [0.0; K];
            for (r, &ir) in sel.iter().enumerate() {
                full[ir] = w[r].max(0.0);
            }
            best = (obj, full);
        }
    }
    best.1
}
/// Per-group weights, trained on parity `train` (None = all rows). Cauchy-IRLS (4 iterations) on the 4x error.
fn fit(data: &Data, mask: usize, lmap: &[usize; 9], ng: usize, train: Option<u8>) -> Vec<[f64; K]> {
    let idx: Vec<usize> = (0..K).filter(|i| mask >> i & 1 == 1).collect();
    let mut w = vec![[0.0; K]; ng];
    for g in 0..ng {
        for &i in &idx {
            w[g][i] = 1.0 / idx.len() as f64;
        }
    }
    if idx.len() == 1 {
        return w;
    }
    for it in 0..IRLS_IT {
        let mut gram = vec![[[0.0f64; K]; K]; ng];
        for p in 0..data.d.len() {
            if let Some(t) = train {
                if data.par[p] != t {
                    continue;
                }
            }
            let g = lmap[data.leaf[p] as usize];
            let d = &data.d[p];
            let om = if it == 0 {
                1.0
            } else {
                let mut r = 0.0;
                for &i in &idx {
                    r += w[g][i] * d[i] as f64;
                }
                1.0 / (CAUCHY_C2 + r * r)
            };
            for (a, &ia) in idx.iter().enumerate() {
                let da = om * d[ia] as f64;
                for &ib in &idx[a..] {
                    gram[g][ia][ib] += da * d[ib] as f64;
                }
            }
        }
        for g in 0..ng {
            if gram[g][idx[0]][idx[0]] > 0.0 {
                w[g] = solve_qp(&gram[g], &idx);
            }
        }
    }
    w
}
fn pcost(r: f64) -> f64 {
    (1.0 + r.abs() / 4.0).log2()
}
struct MaskRes {
    mask: usize,
    ho_blk_leaf5: Vec<[f64; 5]>,
    ho_leaf9: [f64; 9],
    in_leaf9: [f64; 9],
    in_leaf5: [f64; 5],
    w_all: Vec<[f64; K]>,
}
fn convex_run(data: &Data, masks: &[usize], lmap: &[usize; 9], ng: usize, nblk: usize) -> Vec<MaskRes> {
    let next = AtomicUsize::new(0);
    let out = Mutex::new(Vec::new());
    let nt = std::thread::available_parallelism().map(|x| x.get()).unwrap_or(4).min(masks.len());
    std::thread::scope(|s| {
        for _ in 0..nt {
            s.spawn(|| loop {
                let t = next.fetch_add(1, Ordering::SeqCst);
                if t >= masks.len() {
                    break;
                }
                let mask = masks[t];
                let w_even = fit(data, mask, lmap, ng, Some(0));
                let w_odd = fit(data, mask, lmap, ng, Some(1));
                let w_all = fit(data, mask, lmap, ng, None);
                let mut ho_blk = vec![[0.0f64; 5]; nblk];
                let mut ho9 = [0.0f64; 9];
                let mut in9 = [0.0f64; 9];
                let mut in5 = [0.0f64; 5];
                for p in 0..data.d.len() {
                    let l9 = data.leaf[p] as usize;
                    let g = lmap[l9];
                    let d = &data.d[p];
                    let ev = |ws: &[f64; K]| {
                        let mut r = 0.0;
                        for i in 0..K {
                            if mask >> i & 1 == 1 {
                                r += ws[i] * d[i] as f64;
                            }
                        }
                        pcost(r)
                    };
                    // Held-out: parity 1 evaluated with weights fit on parity 0, and vice versa.
                    let c_ho = if data.par[p] == 1 { ev(&w_even[g]) } else { ev(&w_odd[g]) };
                    ho_blk[data.blk[p] as usize][leaf5(data.leaf[p])] += c_ho;
                    ho9[l9] += c_ho;
                    let c_in = ev(&w_all[g]);
                    in9[l9] += c_in;
                    in5[leaf5(data.leaf[p])] += c_in;
                }
                out.lock().unwrap().push(MaskRes { mask, ho_blk_leaf5: ho_blk, ho_leaf9: ho9, in_leaf9: in9, in_leaf5: in5, w_all });
            });
        }
    });
    let mut v = out.into_inner().unwrap();
    v.sort_by_key(|r| r.mask);
    v
}

// ------------------------------------------------------------------------------ supplementary: complementarity BEYOND the production blend
fn simplex(m: usize, dd: usize) -> Vec<Vec<f64>> {
    fn rec(m: usize, left: usize, dd: usize, cur: &mut Vec<f64>, out: &mut Vec<Vec<f64>>) {
        if m == 1 {
            cur.push(left as f64 / dd as f64);
            out.push(cur.clone());
            cur.pop();
            return;
        }
        for u in 0..=left {
            cur.push(u as f64 / dd as f64);
            rec(m - 1, left - u, dd, cur, out);
            cur.pop();
        }
    }
    let mut out = vec![];
    rec(m, dd, dd, &mut vec![], &mut out);
    out
}
/// Columns: 0 = production blend error, 1..=5 = experts H,V,D',Paeth,Smooth. Fixed per-leaf convex mix of the chosen columns on a
/// simplex grid, grid point chosen on one row parity and scored on the other. Returns held-out cost per (block, leaf5) and the
/// all-rows-chosen weights per leaf.
fn grid_study(data: &Data, cols: &[usize], dd: usize, nblk: usize) -> (Vec<[f64; 5]>, Vec<Vec<f64>>) {
    let grid = simplex(cols.len(), dd);
    let ng = grid.len();
    let val = |p: usize, c: usize| -> f64 { if c == 0 { data.dprod[p] as f64 } else { data.d[p][c - 1] as f64 } };
    let mut sums = vec![vec![vec![0.0f64; ng]; 5]; 2];
    for p in 0..data.d.len() {
        let l = leaf5(data.leaf[p]);
        let v: Vec<f64> = cols.iter().map(|&c| val(p, c)).collect();
        for (gi, g) in grid.iter().enumerate() {
            let r: f64 = g.iter().zip(&v).map(|(a, b)| a * b).sum();
            sums[data.par[p] as usize][l][gi] += pcost(r);
        }
    }
    let argmin = |v: &[f64]| v.iter().enumerate().min_by(|a, b| a.1.partial_cmp(b.1).unwrap()).unwrap().0;
    let best_t: Vec<Vec<usize>> = (0..2).map(|t| (0..5).map(|l| argmin(&sums[t][l])).collect()).collect();
    let all_w: Vec<Vec<f64>> = (0..5).map(|l| grid[argmin(&(0..ng).map(|g| sums[0][l][g] + sums[1][l][g]).collect::<Vec<_>>())].clone()).collect();
    let mut ho = vec![[0.0f64; 5]; nblk];
    for p in 0..data.d.len() {
        let l = leaf5(data.leaf[p]);
        let g = &grid[best_t[1 - data.par[p] as usize][l]];
        let r: f64 = g.iter().zip(cols).map(|(a, &c)| a * val(p, c)).sum();
        ho[data.blk[p] as usize][l] += pcost(r);
    }
    (ho, all_w)
}

// ------------------------------------------------------------------------------ main analysis
const NEXP: usize = 7; // 5 experts, production blend, Smooth-q4
fn blk_len() -> usize {
    5 * NEXP + 5 + 5 * 32 + 5 * 32
}
fn o_exp(l: usize, e: usize) -> usize {
    l * NEXP + e
}
fn o_cnt(l: usize) -> usize {
    5 * NEXP + l
}
fn o_orc(l: usize, m: usize) -> usize {
    5 * NEXP + 5 + l * 32 + m
}
fn o_cvx(l: usize, m: usize) -> usize {
    5 * NEXP + 5 + 160 + l * 32 + m
}
fn name_of(mask: usize) -> String {
    (0..K).filter(|i| mask >> i & 1 == 1).map(|i| NAMES[i]).collect::<Vec<_>>().join("+")
}
fn sel_sum(a: &[f64], leaves: &[usize], f: impl Fn(usize) -> usize) -> f64 {
    leaves.iter().map(|&l| a[f(l)]).sum()
}
/// All headline metrics from an aggregated block vector, over a set of leaves.
fn metrics(a: &[f64], leaves: &[usize], pre: &str) -> Vec<(String, f64)> {
    let mut v = vec![];
    let ex = |e: usize| sel_sum(a, leaves, |l| o_exp(l, e));
    let orc = |m: usize| sel_sum(a, leaves, |l| o_orc(l, m));
    let cvx = |m: usize| sel_sum(a, leaves, |l| o_cvx(l, m));
    let prod = ex(5);
    let n: f64 = sel_sum(a, leaves, o_cnt);
    v.push((format!("{pre}pixels"), n));
    v.push((format!("{pre}prod_bits_per_px"), prod / n));
    for e in 0..K {
        v.push((format!("{pre}expert[{}] vs prod %", NAMES[e]), 100.0 * (ex(e) / prod - 1.0)));
    }
    v.push((format!("{pre}Smooth-q4 vs prod %"), 100.0 * (ex(6) / prod - 1.0)));
    v.push((format!("{pre}convex HVD vs prod %"), 100.0 * (cvx(HVD) / prod - 1.0)));
    v.push((format!("{pre}convex full vs prod %"), 100.0 * (cvx(FULL) / prod - 1.0)));
    v.push((format!("{pre}convex full vs convex HVD %"), 100.0 * (cvx(FULL) / cvx(HVD) - 1.0)));
    v.push((format!("{pre}oracle full vs prod %"), 100.0 * (orc(FULL) / prod - 1.0)));
    v.push((format!("{pre}oracle HVD vs prod %"), 100.0 * (orc(HVD) / prod - 1.0)));
    for (what, f) in [("convex", &cvx as &dyn Fn(usize) -> f64), ("oracle", &orc as &dyn Fn(usize) -> f64)] {
        for e in 0..K {
            // Leave-one-out loss relative to the full roster's cost of the same kind.
            v.push((format!("{pre}{what} LOO[{}] %", NAMES[e]), 100.0 * (f(FULL & !(1 << e)) / f(FULL) - 1.0)));
        }
        for e in [3usize, 4] {
            v.push((format!("{pre}{what} add {} to HVD %", NAMES[e]), 100.0 * (1.0 - f(HVD | 1 << e) / f(HVD))));
        }
        v.push((format!("{pre}{what} add Paeth+Smooth to HVD %"), 100.0 * (1.0 - f(FULL) / f(HVD))));
    }
    v
}

fn pct(v: &mut Vec<f64>, q: f64) -> f64 {
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    v[((v.len() - 1) as f64 * q).round() as usize]
}

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

fn main() {
    if cfg!(debug_assertions) {
        eprintln!("use --release");
        std::process::exit(2);
    }
    let a: Vec<String> = std::env::args().collect();
    let out = PathBuf::from(a.iter().position(|x| x == "--out").map(|i| a[i + 1].clone()).expect("--out <dir>"));
    std::fs::create_dir_all(&out).unwrap();
    let names = ["portrait_0", "screen_0", "night_0"];
    let files = ["portrait_0.jpg", "screen_0.png", "night_0.jpg"];
    let mut data = Data::default();
    let mut calib: Vec<Calib> = (0..3).map(|_| Calib::new()).collect();
    let mut verified = 0u64;
    let mut notes = String::new();
    for (ii, f) in files.iter().enumerate() {
        let img = center_crop(&Canonical::load(&PathBuf::from("data").join(f)).unwrap(), 1024);
        let (w, h) = (img.spec.width as usize, img.spec.height as usize);
        let prod = pkon::encode_standard_with(&img, EllieRepr::Auto).unwrap();
        let hdr = pkon::parse_header(&prod).unwrap();
        let CodecParams::Ellie(p) = &hdr.params else { panic!("not ELLIE") };
        let ycocg = p.representation == ELLIE_REPR_YCOCG_R;
        let planes = split_planes(&img, ycocg);
        for (k, pl) in planes.iter().enumerate() {
            let (mn, mx) = (pl.iter().min().unwrap(), pl.iter().max().unwrap());
            if mn == mx {
                writeln!(notes, "{}: plane {k} is constant ({mn}); excluded", names[ii]).unwrap();
                continue;
            }
            run_plane(pl, w, h, p.planes[k].0 as u32, ii as u8, &mut data, &mut calib[ii], &mut verified);
        }
        writeln!(notes, "{}: {w}x{h}, ycocg={ycocg}, planes={}", names[ii], planes.len()).unwrap();
    }
    let n = data.d.len();
    let nblk = data.blk_img.len();
    println!("pixels analysed {n}, blocks {nblk}, copied Stage 1 verified against production on {verified} pixels");

    // ---- single pass: per-expert costs, per-pixel oracles, moments, wins, leaf counts
    let ctab: Vec<f32> = (0..2200).map(|v| pcost(v as f64) as f32).collect();
    let cost = |d: i16| ctab[d.unsigned_abs() as usize] as f64;
    let mut blkv = vec![vec![0.0f64; blk_len()]; nblk];
    let mut sums_par = vec![[[0.0f64; K]; 9]; 2]; // [parity][leaf9][expert]
    let mut leafcnt = vec![[0u64; 9]; 3];
    let mut wins = vec![[0.0f64; K]; 5];
    // moments: [leaf5][a][b] of signed d and |d|, plus first moments
    let mut m_s = vec![([[0.0f64; K]; K], [0.0f64; K], 0.0f64); 5];
    let mut m_a = vec![([[0.0f64; K]; K], [0.0f64; K], 0.0f64); 5];
    let mut mae = vec![[[0.0f64; K + 2]; 5]; 3]; // [img][leaf5][expert,prod,q4] sum |d|/4 ; (used for MAE/bias)
    let mut bias = vec![[[0.0f64; K]; 5]; 3];
    let mut sq = vec![[[0.0f64; K]; 5]; 3];
    let mut cnt_img_leaf = vec![[0f64; 5]; 3];
    for p in 0..n {
        let d = &data.d[p];
        let l9 = data.leaf[p] as usize;
        let l5 = leaf5(data.leaf[p]);
        let b = data.blk[p] as usize;
        let im = data.img[p] as usize;
        let c: [f64; K] = std::array::from_fn(|i| cost(d[i]));
        let blk = &mut blkv[b];
        for i in 0..K {
            blk[o_exp(l5, i)] += c[i];
            sums_par[data.par[p] as usize][l9][i] += c[i];
        }
        blk[o_exp(l5, 5)] += cost(data.dprod[p]);
        blk[o_exp(l5, 6)] += cost(data.dq4[p]);
        blk[o_cnt(l5)] += 1.0;
        let mut minc = [f64::INFINITY; 32];
        for mask in 1..32usize {
            let lb = mask.trailing_zeros() as usize;
            let rest = mask & (mask - 1);
            minc[mask] = if rest == 0 { c[lb] } else { minc[rest].min(c[lb]) };
            blk[o_orc(l5, mask)] += minc[mask];
        }
        leafcnt[im][l9] += 1;
        cnt_img_leaf[im][l5] += 1.0;
        let mut best = u16::MAX;
        for i in 0..K {
            best = best.min(d[i].unsigned_abs());
        }
        let nt = (0..K).filter(|&i| d[i].unsigned_abs() == best).count() as f64;
        for i in 0..K {
            if d[i].unsigned_abs() == best {
                wins[l5][i] += 1.0 / nt;
            }
        }
        let df: [f64; K] = std::array::from_fn(|i| d[i] as f64);
        let af: [f64; K] = std::array::from_fn(|i| df[i].abs());
        for i in 0..K {
            m_s[l5].1[i] += df[i];
            m_a[l5].1[i] += af[i];
            bias[im][l5][i] += df[i] / 4.0;
            sq[im][l5][i] += (df[i] / 4.0) * (df[i] / 4.0);
            mae[im][l5][i] += af[i] / 4.0;
            for j in i..K {
                m_s[l5].0[i][j] += df[i] * df[j];
                m_a[l5].0[i][j] += af[i] * af[j];
            }
        }
        mae[im][l5][5] += (data.dprod[p] as f64).abs() / 4.0;
        mae[im][l5][6] += (data.dq4[p] as f64).abs() / 4.0;
        m_s[l5].2 += 1.0;
        m_a[l5].2 += 1.0;
    }

    // ---- convex fits (held-out even/odd, plus in-sample for the overfit gap)
    let l5map: [usize; 9] = std::array::from_fn(|l| leaf5(l as u8));
    let id9: [usize; 9] = std::array::from_fn(|l| l);
    let all_masks: Vec<usize> = (1..32).collect();
    println!("fitting convex combinations (5-leaf tree, 31 subsets) ...");
    let r5 = convex_run(&data, &all_masks, &l5map, 5, nblk);
    let masks9: Vec<usize> = std::iter::once(HVD).chain(std::iter::once(FULL)).chain((0..K).map(|e| FULL & !(1 << e))).collect();
    println!("fitting convex combinations (9-leaf diagnostic, 7 subsets) ...");
    let r9 = convex_run(&data, &masks9, &id9, 9, nblk);
    for r in &r5 {
        for (b, v) in r.ho_blk_leaf5.iter().enumerate() {
            for l in 0..5 {
                blkv[b][o_cvx(l, r.mask)] = v[l];
            }
        }
    }

    // ---- supplementary: does an expert add information on top of the production blend?
    println!("supplementary grid studies ...");
    let studies: Vec<(String, Vec<usize>, usize)> = {
        let mut s: Vec<(String, Vec<usize>, usize)> = (1..=K).map(|e| (format!("prod + {}", NAMES[e - 1]), vec![0, e], 20)).collect();
        s.push(("prod + Paeth + Smooth".into(), vec![0, 4, 5], 10));
        s.push(("prod + Paeth".into(), vec![0, 4], 20));
        s
    };
    let sup: Vec<(Vec<[f64; 5]>, Vec<Vec<f64>>)> = std::thread::scope(|s| {
        let hs: Vec<_> = studies.iter().map(|(_, cols, dd)| { let (d, c, dd) = (&data, cols.clone(), *dd); s.spawn(move || grid_study(d, &c, dd, nblk)) }).collect();
        hs.into_iter().map(|h| h.join().unwrap()).collect()
    });
    // correlation of production's signed error with each expert's, per leaf5 and pooled
    let mut pc = vec![vec![Corr::default(); K]; 5];
    for p in 0..n {
        let l = leaf5(data.leaf[p]);
        for e in 0..K {
            pc[l][e].add(data.dprod[p] as f64, data.d[p][e] as f64);
        }
    }

    // ---- aggregates and bootstrap
    let agg_of = |blocks: &[usize]| -> Vec<f64> {
        let mut a = vec![0.0; blk_len()];
        for &b in blocks {
            for (x, y) in a.iter_mut().zip(&blkv[b]) {
                *x += y;
            }
        }
        a
    };
    let scopes: Vec<(String, Vec<usize>)> = {
        let mut s = vec![("ALL".to_string(), (0..nblk).collect::<Vec<_>>())];
        for (i, nm) in names.iter().enumerate() {
            s.push((nm.to_string(), (0..nblk).filter(|&b| data.blk_img[b] as usize == i).collect()));
        }
        s
    };
    let all5: Vec<usize> = (0..5).collect();
    let mut seed = 0x9E3779B97F4A7C15u64;
    let mut rng = move || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    // point estimates + CI per scope
    let mut md = String::new();
    let mut csv = String::from("scope,leaf,metric,value,ci_lo,ci_hi\n");
    let mut cis: Vec<(String, Vec<(String, f64, f64, f64)>)> = vec![];
    let leaf_scopes: Vec<(String, Vec<usize>)> = std::iter::once(("all-leaves".to_string(), all5.clone())).chain((0..5).map(|l| (LN5[l].to_string(), vec![l]))).collect();
    for (sn, blocks) in &scopes {
        let pt_all = agg_of(blocks);
        let mut rows = vec![];
        let mut samples: Vec<Vec<Vec<(String, f64)>>> = vec![]; // [rep][leafscope] -> metrics
        for _ in 0..BOOT {
            let rb: Vec<usize> = (0..blocks.len()).map(|_| blocks[(rng() % blocks.len() as u64) as usize]).collect();
            let ag = agg_of(&rb);
            samples.push(leaf_scopes.iter().map(|(ln, lv)| metrics(&ag, lv, &format!("{ln}|"))).collect());
        }
        for (si, (ln, lv)) in leaf_scopes.iter().enumerate() {
            if sn != "ALL" && ln != "all-leaves" {
                continue;
            }
            let pt = metrics(&pt_all, lv, &format!("{ln}|"));
            for (mi, (name, val)) in pt.iter().enumerate() {
                let mut xs: Vec<f64> = samples.iter().map(|s| s[si][mi].1).filter(|x| x.is_finite()).collect();
                let (lo, hi) = if xs.len() > 20 { (pct(&mut xs.clone(), 0.025), pct(&mut xs, 0.975)) } else { (f64::NAN, f64::NAN) };
                rows.push((name.clone(), *val, lo, hi));
                writeln!(csv, "{sn},{ln},{},{val},{lo},{hi}", name.split('|').nth(1).unwrap()).unwrap();
            }
        }
        cis.push((sn.clone(), rows));
    }
    let get = |scope: &str, key: &str| -> (f64, f64, f64) {
        cis.iter().find(|c| c.0 == scope).unwrap().1.iter().find(|r| r.0 == key).map(|r| (r.1, r.2, r.3)).unwrap_or_else(|| panic!("no metric {key}"))
    };
    let fmt3 = |t: (f64, f64, f64)| format!("{:+.3} [{:+.3}, {:+.3}]", t.0, t.1, t.2);

    // ---- report
    writeln!(md, "# ELLIE predictor swarm, step 1: expert-diversity analysis (offline, canary only)\n").unwrap();
    writeln!(md, "Pixels analysed: {n} (rows>=1, cols>=1; excludes row 0 and column 0). Copied Stage 1 verified equal to the live production prediction plane on all {verified} analysed pixels, and H/V/D' + Paeth reproduce production's H4/V4/D4 estimates exactly (asserted per pixel).\n```\n{notes}```\n").unwrap();
    writeln!(md, "Units: errors in 1/4-pixel units; bit proxy per pixel = log2(1+|r|/4). 'vs prod' compares with the live production blend (negative = better). CI = 95% row-block bootstrap ({BOOT} reps, {BLK_ROWS}-row blocks); it reflects evaluation noise only, NOT the uncertainty of the fitted weights (fits are done once).\n").unwrap();
    writeln!(md, "## 1. Pixel-only tree: leaf population (share of analysed pixels)\n").unwrap();
    writeln!(md, "| leaf9 | leaf5 | portrait | screen | night | ALL |\n|---|---|---|---|---|---|").unwrap();
    let l9n = ["flat", "two-level", "smooth", "coh/g<64", "coh/g<256", "coh/g>=256", "incoh/g<64", "incoh/g<256", "incoh/g>=256"];
    let tot_img: Vec<u64> = (0..3).map(|i| leafcnt[i].iter().sum()).collect();
    for l in 0..9 {
        let all: u64 = (0..3).map(|i| leafcnt[i][l]).sum();
        writeln!(md, "| {} | {} | {:.1}% | {:.1}% | {:.1}% | {:.1}% |", l9n[l], LN5[leaf5(l as u8)], 100.0 * leafcnt[0][l] as f64 / tot_img[0] as f64, 100.0 * leafcnt[1][l] as f64 / tot_img[1] as f64, 100.0 * leafcnt[2][l] as f64 / tot_img[2] as f64, 100.0 * all as f64 / n as f64).unwrap();
    }
    let leaf_share: Vec<f64> = (0..5).map(|l| (0..3).map(|i| cnt_img_leaf[i][l]).sum::<f64>() / n as f64).collect();

    writeln!(md, "\n## 2. Individual expert error (aggregate, all leaves)\n").unwrap();
    writeln!(md, "| expert | MAE px (portrait / screen / night / ALL) | RMSE px ALL | bias px ALL | bit proxy/px ALL | vs prod % [CI] |\n|---|---|---|---|---|---|").unwrap();
    for e in 0..K + 2 {
        let nm = if e < K { NAMES[e].to_string() } else if e == 5 { "production blend".into() } else { "Smooth-q4 (precision check, not roster)".into() };
        let mut cells = vec![];
        for im in 0..3 {
            let s: f64 = (0..5).map(|l| mae[im][l][e]).sum();
            cells.push(format!("{:.3}", s / tot_img[im] as f64));
        }
        let sall: f64 = (0..3).map(|im| (0..5).map(|l| mae[im][l][e]).sum::<f64>()).sum();
        cells.push(format!("{:.3}", sall / n as f64));
        let (rm, bi, bp) = if e < K {
            let sq_all: f64 = (0..3).map(|im| (0..5).map(|l| sq[im][l][e]).sum::<f64>()).sum();
            let b_all: f64 = (0..3).map(|im| (0..5).map(|l| bias[im][l][e]).sum::<f64>()).sum();
            (format!("{:.3}", (sq_all / n as f64).sqrt()), format!("{:+.3}", b_all / n as f64), 0.0)
        } else {
            ("-".into(), "-".into(), 0.0)
        };
        let _ = bp;
        let key = if e < K { format!("all-leaves|expert[{}] vs prod %", NAMES[e]) } else if e == 6 { "all-leaves|Smooth-q4 vs prod %".into() } else { String::new() };
        let bits: f64 = (0..5).map(|l| (0..blkv.len()).map(|b| blkv[b][o_exp(l, e)]).sum::<f64>()).sum();
        writeln!(md, "| {nm} | {} / {} / {} / {} | {rm} | {bi} | {:.4} | {} |", cells[0], cells[1], cells[2], cells[3], bits / n as f64, if key.is_empty() { "-".into() } else { fmt3(get("ALL", &key)) }).unwrap();
    }
    writeln!(md, "\nPer image, expert bit proxy vs production (%):\n\n| expert | portrait | screen | night |\n|---|---|---|---|").unwrap();
    for e in 0..K {
        let key = format!("all-leaves|expert[{}] vs prod %", NAMES[e]);
        writeln!(md, "| {} | {} | {} | {} |", NAMES[e], fmt3(get("portrait_0", &key)), fmt3(get("screen_0", &key)), fmt3(get("night_0", &key))).unwrap();
    }

    writeln!(md, "\n## 3. Per-leaf expert accuracy (bit proxy vs production, %; plus how often each expert has the smallest |error|)\n").unwrap();
    writeln!(md, "| leaf5 (pixel share) | H | V | D' | Paeth | Smooth | prod bits/px | winner shares H/V/D'/Paeth/Smooth |\n|---|---|---|---|---|---|---|---|").unwrap();
    for l in 0..5 {
        let mut cells = vec![];
        for e in 0..K {
            cells.push(format!("{:+.2}", get("ALL", &format!("{}|expert[{}] vs prod %", LN5[l], NAMES[e])).0));
        }
        let pb = get("ALL", &format!("{}|prod_bits_per_px", LN5[l])).0;
        let ws: Vec<String> = (0..K).map(|e| format!("{:.0}", 100.0 * wins[l][e] / (leaf_share[l] * n as f64))).collect();
        writeln!(md, "| {} ({:.1}%) | {} | {:.3} | {} |", LN5[l], 100.0 * leaf_share[l], cells.join(" | "), pb, ws.join("/")).unwrap();
    }

    writeln!(md, "\n## 4. Pairwise error correlation\n").unwrap();
    let corr_of = |l: &[usize], signed: bool| -> [[f64; K]; K] {
        let mut s = [[0.0; K]; K];
        let mut f = [0.0; K];
        let mut nn = 0.0;
        for &li in l {
            let m = if signed { &m_s[li] } else { &m_a[li] };
            for i in 0..K {
                f[i] += m.1[i];
                for j in i..K {
                    s[i][j] += m.0[i][j];
                }
            }
            nn += m.2;
        }
        let mut r = [[1.0; K]; K];
        for i in 0..K {
            for j in i..K {
                let cov = s[i][j] / nn - f[i] / nn * f[j] / nn;
                let vi = s[i][i] / nn - (f[i] / nn).powi(2);
                let vj = s[j][j] / nn - (f[j] / nn).powi(2);
                r[i][j] = cov / (vi * vj).sqrt();
                r[j][i] = r[i][j];
            }
        }
        r
    };
    let mut max_corr_leaf = vec![[[0.0f64; K]; K]; 5];
    for (title, signed) in [("signed error", true), ("|error|", false)] {
        writeln!(md, "{title}, all leaves pooled:\n\n| | H | V | D' | Paeth | Smooth |\n|---|---|---|---|---|---|").unwrap();
        let c = corr_of(&all5, signed);
        for i in 0..K {
            writeln!(md, "| {} | {} |", NAMES[i], (0..K).map(|j| format!("{:.3}", c[i][j])).collect::<Vec<_>>().join(" | ")).unwrap();
        }
        writeln!(md).unwrap();
    }
    writeln!(md, "Signed-error correlation by leaf (upper triangle): pairs with r > 0.95 are flagged with `!`.\n\n| leaf5 | HV | HD' | HP | HS | VD' | VP | VS | D'P | D'S | PS |\n|---|---|---|---|---|---|---|---|---|---|---|").unwrap();
    for l in 0..5 {
        let c = corr_of(&[l], true);
        max_corr_leaf[l] = c;
        let mut cells = vec![];
        for i in 0..K {
            for j in i + 1..K {
                cells.push(format!("{:.2}{}", c[i][j], if c[i][j] > 0.95 { "!" } else { "" }));
            }
        }
        writeln!(md, "| {} | {} |", LN5[l], cells.join(" | ")).unwrap();
    }

    writeln!(md, "\n## 5. Oracle and convex-combination bounds (DIAGNOSTIC UPPER BOUNDS, not compression claims)\n").unwrap();
    writeln!(md, "- **Per-pixel oracle** picks the luckiest expert at every pixel; it rewards noise-picking even for experts with no real information, so treat it as the loosest bound.\n- **Convex held-out**: per-leaf non-negative weights summing to 1, fitted (Cauchy-IRLS, c=1px) on even rows and evaluated on odd rows, and vice versa. Rows interleave spatially, so adjacent-row leakage makes this somewhat optimistic.\n- **Convex in-sample** is shown only to quantify overfit.\n").unwrap();
    let total_of = |mask: usize, kind: usize| -> f64 {
        // kind 0 oracle, 1 convex held-out
        (0..nblk).map(|b| (0..5).map(|l| blkv[b][if kind == 0 { o_orc(l, mask) } else { o_cvx(l, mask) }]).sum::<f64>()).sum::<f64>()
    };
    let prod_total: f64 = (0..nblk).map(|b| (0..5).map(|l| blkv[b][o_exp(l, 5)]).sum::<f64>()).sum();
    writeln!(md, "| set | per-pixel oracle vs prod % | convex held-out vs prod % | convex in-sample vs prod % |\n|---|---|---|---|").unwrap();
    let in_total = |mask: usize| -> f64 { r5.iter().find(|r| r.mask == mask).unwrap().in_leaf5.iter().sum() };
    for mask in [0b00001usize, 0b00010, 0b00100, 0b01000, 0b10000, HVD, HVD | 8, HVD | 16, FULL, FULL & !1, FULL & !2, FULL & !4, FULL & !8, FULL & !16] {
        writeln!(md, "| {} | {:+.2} | {:+.2} | {:+.2} |", name_of(mask), 100.0 * (total_of(mask, 0) / prod_total - 1.0), 100.0 * (total_of(mask, 1) / prod_total - 1.0), 100.0 * (in_total(mask) / prod_total - 1.0)).unwrap();
    }
    // static best single expert per leaf, held-out
    let stat_single = |lm: &dyn Fn(usize) -> usize, ng: usize, mask: usize| -> f64 {
        let mut tot = 0.0;
        for g in 0..ng {
            for t in 0..2usize {
                let mut best = (f64::INFINITY, 0);
                for e in (0..K).filter(|e| mask >> e & 1 == 1) {
                    let c: f64 = (0..9).filter(|&l| lm(l) == g).map(|l| sums_par[t][l][e]).sum();
                    if c < best.0 {
                        best = (c, e);
                    }
                }
                tot += (0..9).filter(|&l| lm(l) == g).map(|l| sums_par[1 - t][l][best.1]).sum::<f64>();
            }
        }
        tot
    };
    writeln!(md, "\nStatic best single expert per leaf (held-out, 5-leaf): full roster {:+.2}% vs prod; HVD only {:+.2}% vs prod.", 100.0 * (stat_single(&|l| leaf5(l as u8), 5, FULL) / prod_total - 1.0), 100.0 * (stat_single(&|l| leaf5(l as u8), 5, HVD) / prod_total - 1.0)).unwrap();
    writeln!(md, "Production blend itself (adaptive Shrub + activity weights) is the 0% reference; a negative convex number means a *fixed* per-leaf blend of these experts beats production's adaptive blend.\n").unwrap();

    writeln!(md, "Fitted weights on all rows (in-sample), full roster, per leaf5:\n\n| leaf5 | H | V | D' | Paeth | Smooth |\n|---|---|---|---|---|---|").unwrap();
    let full5 = r5.iter().find(|r| r.mask == FULL).unwrap();
    for l in 0..5 {
        writeln!(md, "| {} | {} |", LN5[l], (0..K).map(|e| format!("{:.2}", full5.w_all[l][e])).collect::<Vec<_>>().join(" | ")).unwrap();
    }

    writeln!(md, "\n## 6. Marginal contribution (95% bootstrap CI), all pixels\n").unwrap();
    writeln!(md, "Leave-one-out = cost increase when the expert is removed from the full roster (positive = the expert helps). Add-to-HVD = cost decrease from adding the expert to {{H,V,D'}}.\n").unwrap();
    writeln!(md, "| expert | convex LOO % | oracle LOO % | convex add-to-HVD % | oracle add-to-HVD % |\n|---|---|---|---|---|").unwrap();
    for e in 0..K {
        let add_c = if e >= 3 { fmt3(get("ALL", &format!("all-leaves|convex add {} to HVD %", NAMES[e]))) } else { "n/a (in HVD)".into() };
        let add_o = if e >= 3 { fmt3(get("ALL", &format!("all-leaves|oracle add {} to HVD %", NAMES[e]))) } else { "n/a (in HVD)".into() };
        writeln!(md, "| {} | {} | {} | {} | {} |", NAMES[e], fmt3(get("ALL", &format!("all-leaves|convex LOO[{}] %", NAMES[e]))), fmt3(get("ALL", &format!("all-leaves|oracle LOO[{}] %", NAMES[e]))), add_c, add_o).unwrap();
    }
    writeln!(md, "\nBoth added: convex {} ; oracle {}.", fmt3(get("ALL", "all-leaves|convex add Paeth+Smooth to HVD %")), fmt3(get("ALL", "all-leaves|oracle add Paeth+Smooth to HVD %"))).unwrap();
    writeln!(md, "\nPer image, convex LOO % (CI) :\n\n| expert | portrait | screen | night |\n|---|---|---|---|").unwrap();
    for e in 0..K {
        let k = format!("all-leaves|convex LOO[{}] %", NAMES[e]);
        writeln!(md, "| {} | {} | {} | {} |", NAMES[e], fmt3(get("portrait_0", &k)), fmt3(get("screen_0", &k)), fmt3(get("night_0", &k))).unwrap();
    }
    writeln!(md, "\nPer leaf, convex LOO % of that leaf's cost (CI), all images:\n\n| expert | {} |\n|---|---|---|---|---|---|", (0..5).map(|l| format!("{} ({:.1}%)", LN5[l], 100.0 * leaf_share[l])).collect::<Vec<_>>().join(" | ")).unwrap();
    for e in 0..K {
        writeln!(md, "| {} | {} |", NAMES[e], (0..5).map(|l| fmt3(get("ALL", &format!("{}|convex LOO[{}] %", LN5[l], NAMES[e])))).collect::<Vec<_>>().join(" | ")).unwrap();
    }
    // greedy forward selection (convex held-out totals)
    writeln!(md, "\nGreedy forward selection (convex held-out total, vs prod %):").unwrap();
    let mut chosen = 0usize;
    for _ in 0..K {
        let mut best = (f64::INFINITY, 0usize);
        for e in (0..K).filter(|e| chosen >> e & 1 == 0) {
            let c = total_of(chosen | 1 << e, 1);
            if c < best.0 {
                best = (c, e);
            }
        }
        chosen |= 1 << best.1;
        writeln!(md, "- + {} -> {} : {:+.3}%", NAMES[best.1], name_of(chosen), 100.0 * (best.0 / prod_total - 1.0)).unwrap();
    }

    writeln!(md, "\n## 7. Option (b): 9-leaf pixel-only tree (diagnostic, convex held-out; gain vs the 5-leaf tree with the same roster)\n").unwrap();
    writeln!(md, "| set | 9-leaf held-out vs prod % | 5-leaf held-out vs prod % |\n|---|---|---|").unwrap();
    for r in &r9 {
        let t9: f64 = r.ho_leaf9.iter().sum();
        writeln!(md, "| {} | {:+.3} | {:+.3} |", name_of(r.mask), 100.0 * (t9 / prod_total - 1.0), 100.0 * (total_of(r.mask, 1) / prod_total - 1.0)).unwrap();
    }
    let f9 = r9.iter().find(|r| r.mask == FULL).unwrap();
    let t9f: f64 = f9.ho_leaf9.iter().sum();
    writeln!(md, "\n9-leaf convex LOO % (relative to the 9-leaf full roster): {}", (0..K).map(|e| { let r = r9.iter().find(|r| r.mask == FULL & !(1 << e)).unwrap(); format!("{} {:+.3}", NAMES[e], 100.0 * (r.ho_leaf9.iter().sum::<f64>() / t9f - 1.0)) }).collect::<Vec<_>>().join(", ")).unwrap();

    writeln!(md, "\n## 8. Local-evidence calibration L = (2eW + 2eN + eNW + eNE) >> 1 (UNVALIDATED)\n").unwrap();
    writeln!(md, "e = each neighbour's own integer-pixel error for that expert, ((|d|+2)>>2). Pixels whose four neighbours are all analysed (rows>=2, cols 2..w-2). S_H/S_V/S_D are the production activity sums (S_D after the `m + (m>>2)` rule). If L is a usable proxy it should (i) sit on a comparable scale to S, (ii) correlate with the expert's own error at the current pixel at least as well as S does (log-log Pearson).\n").unwrap();
    let mut cal = Calib::new();
    for c in &calib {
        cal.merge(c);
    }
    writeln!(md, "| quantity | mean | median |\n|---|---|---|").unwrap();
    for (i, nm) in ["S_H", "S_V", "S_D"].iter().enumerate() {
        writeln!(md, "| {nm} (production) | {:.2} | {} |", cal.s_sum[i] / cal.n, hist_median(&cal.s_hist[i])).unwrap();
    }
    for i in 0..K {
        writeln!(md, "| L_{} | {:.2} | {} |", NAMES[i], cal.l_sum[i] / cal.n, hist_median(&cal.l_hist[i])).unwrap();
    }
    writeln!(md, "\n(histograms are clamped at {}; means are unclamped.)\n\n| expert | corr(logL, log own err) | corr(logS_own, log own err) | corr(logL, logS_own) |\n|---|---|---|---|", HB - 1).unwrap();
    for i in 0..K {
        let (cs, cl) = if i < 3 { (format!("{:.3}", cal.cse[i].r()), format!("{:.3}", cal.cls[i].r())) } else { (format!("n/a; log(S_H+S_V): {:.3}", cal.cge[i - 3].r()), "n/a".into()) };
        writeln!(md, "| {} | {:.3} | {} | {} |", NAMES[i], cal.cle[i].r(), cs, cl).unwrap();
    }
    writeln!(md, "\nPer image corr(logL, log own err) / corr(logS, log own err) for H, V, D':\n\n| expert | portrait | screen | night |\n|---|---|---|---|").unwrap();
    for i in 0..3 {
        writeln!(md, "| {} | {} |", NAMES[i], (0..3).map(|im| format!("{:.3} / {:.3}", calib[im].cle[i].r(), calib[im].cse[i].r())).collect::<Vec<_>>().join(" | ")).unwrap();
    }

    // ---- supplementary report
    writeln!(md, "\n## 10. Supplementary: information beyond the production blend (added after the first run exposed a gap)\n").unwrap();
    writeln!(md, "Section 5's fixed per-leaf blends cannot use production's per-pixel activity weighting, so they cannot say whether Paeth or Smooth add anything to the *existing* adaptive blend. Here the production blend's own prediction is a candidate: a fixed per-leaf convex mix of production + expert(s) is searched on a simplex grid (step 0.05 for one expert, 0.1 for two), chosen on one row parity and scored on the other. 'Gain' = cost reduction vs production alone (positive = the expert adds information the production blend lacks). Weights are the all-rows choice (production weight / expert weight(s)).\n").unwrap();
    let mut rng2 = 0x1234567u64;
    let mut rr = move || {
        rng2 ^= rng2 << 13;
        rng2 ^= rng2 >> 7;
        rng2 ^= rng2 << 17;
        rng2
    };
    let blocks_all: Vec<usize> = (0..nblk).collect();
    let gain_of = |ho: &Vec<[f64; 5]>, bl: &[usize], lv: &[usize]| -> f64 {
        let (mut a, mut b) = (0.0, 0.0);
        for &x in bl {
            for &l in lv {
                a += ho[x][l];
                b += blkv[x][o_exp(l, 5)];
            }
        }
        100.0 * (1.0 - a / b)
    };
    writeln!(md, "| study | gain all leaves % [CI] | portrait | screen | night | weights per leaf5 (prod/expert...) |\n|---|---|---|---|---|---|").unwrap();
    for (si, (nm, _, _)) in studies.iter().enumerate() {
        let ho = &sup[si].0;
        let mut bs = vec![];
        for _ in 0..BOOT {
            let rb: Vec<usize> = (0..nblk).map(|_| (rr() % nblk as u64) as usize).collect();
            bs.push(gain_of(ho, &rb, &all5));
        }
        let (lo, hi) = (pct(&mut bs.clone(), 0.025), pct(&mut bs, 0.975));
        let per_img: Vec<String> = (0..3).map(|im| { let b: Vec<usize> = (0..nblk).filter(|&b| data.blk_img[b] as usize == im).collect(); format!("{:+.2}", gain_of(ho, &b, &all5)) }).collect();
        let w: Vec<String> = (0..5).map(|l| sup[si].1[l].iter().map(|x| format!("{x:.2}")).collect::<Vec<_>>().join("/")).collect();
        writeln!(md, "| {nm} | {:+.3} [{:+.3}, {:+.3}] | {} | {} | {} | {} |", gain_of(ho, &blocks_all, &all5), lo, hi, per_img[0], per_img[1], per_img[2], w.join(" ; ")).unwrap();
    }
    writeln!(md, "\n(weights order: flat ; two-level ; smooth ; cont-coherent ; cont-incoherent)\n\nGain by leaf (%), for the two-expert and triple studies:\n\n| study | {} |\n|---|---|---|---|---|---|", LN5.join(" | ")).unwrap();
    for (si, (nm, _, _)) in studies.iter().enumerate() {
        let cells: Vec<String> = (0..5).map(|l| format!("{:+.2}", gain_of(&sup[si].0, &blocks_all, &[l]))).collect();
        writeln!(md, "| {nm} | {} |", cells.join(" | ")).unwrap();
    }
    writeln!(md, "\nSigned-error correlation between the production blend and each expert:\n\n| leaf5 | H | V | D' | Paeth | Smooth |\n|---|---|---|---|---|---|").unwrap();
    for l in 0..5 {
        writeln!(md, "| {} | {} |", LN5[l], (0..K).map(|e| format!("{:.3}", pc[l][e].r())).collect::<Vec<_>>().join(" | ")).unwrap();
    }
    let mut pooled = vec![Corr::default(); K];
    for l in 0..5 {
        for e in 0..K {
            pooled[e].merge(&pc[l][e]);
        }
    }
    writeln!(md, "| pooled | {} |", (0..K).map(|e| format!("{:.3}", pooled[e].r())).collect::<Vec<_>>().join(" | ")).unwrap();

    // ---- flags (diagnostic, not automatic deletion rules)
    writeln!(md, "\n## 9. Diagnostic flags (user-specified thresholds; evidence, not deletion rules)\n").unwrap();
    writeln!(md, "| expert | convex LOO total <0.1%? | all leaves >=5% pixels have LOO <0.5%? | max pair corr >0.95 (pooled) | LOO loss share from leaves <5% pixels |\n|---|---|---|---|---|").unwrap();
    let cpool = corr_of(&all5, true);
    for e in 0..K {
        let tot = get("ALL", &format!("all-leaves|convex LOO[{}] %", NAMES[e])).0;
        let big: Vec<usize> = (0..5).filter(|&l| leaf_share[l] >= 0.05).collect();
        let all_small = big.iter().all(|&l| get("ALL", &format!("{}|convex LOO[{}] %", LN5[l], NAMES[e])).0 < 0.5);
        let mut mc = (0.0f64, 0usize);
        for j in 0..K {
            if j != e && cpool[e][j] > mc.0 {
                mc = (cpool[e][j], j);
            }
        }
        // share of the absolute LOO loss (bits) coming from small leaves
        let loss_l: Vec<f64> = (0..5).map(|l| get("ALL", &format!("{}|convex LOO[{}] %", LN5[l], NAMES[e])).0 / 100.0 * (0..nblk).map(|b| blkv[b][o_cvx(l, FULL)]).sum::<f64>()).collect();
        let pos: f64 = loss_l.iter().filter(|v| **v > 0.0).sum();
        let small: f64 = (0..5).filter(|&l| leaf_share[l] < 0.05).map(|l| loss_l[l].max(0.0)).sum();
        writeln!(md, "| {} | {} ({:+.3}%) | {} | {} ({:.3} with {}) | {} |", NAMES[e], if tot < 0.1 { "YES" } else { "no" }, tot, if all_small { "YES (below 0.5% everywhere)" } else { "no" }, if mc.0 > 0.95 { "YES" } else { "no" }, mc.0, NAMES[mc.1], if (0..5).all(|l| leaf_share[l] >= 0.05) { "n/a (every leaf >=5%)".to_string() } else if pos > 0.0 { format!("{:.0}%", 100.0 * small / pos) } else { "0%".into() }).unwrap();
    }
    writeln!(md, "\nPer-leaf pairs above 0.95 are marked `!` in section 4.").unwrap();
    std::fs::write(out.join("summary.md"), &md).unwrap();
    std::fs::write(out.join("metrics_ci.csv"), &csv).unwrap();
    println!("{md}");
}
