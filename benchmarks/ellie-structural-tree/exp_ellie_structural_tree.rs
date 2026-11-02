//! Structural-tree experiment for ELLIE (isolated; production untouched). See DESIGN.md.
//!
//! One fused per-sample scan (copy of the production Stage 1 scan with a causal "structural tree" hook) feeds a copy of the
//! production Stage 3 BC model (with optional tree-driven context / mixer-select / per-leaf Shrub) and the production Stage 4
//! range coder. With every tree option off the plane streams must equal the production ones byte for byte (checked).
//!   cargo run --release --bin exp_ellie_structural_tree -- [--out <dir>] [--reps N]
use nanoscope::canon::{Canonical, Spec};
use nanoscope::ellie::{self, Stage4Dec, Stage4Enc};
use nanoscope::pkon::{self, CodecParams, EllieRepr, ELLIE_REPR_YCOCG_R};
use nanoscope::rangecoder::BitIo;
use std::path::{Path, PathBuf};
use std::time::Instant;

const CROP: u32 = 1024;
const NCLS: usize = 16;

// ------------------------------------------------------------------------------------------------ tree

#[derive(Clone, Copy)]
struct TreeCfg {
    flat: bool, // value-diversity (flat / two-level / smooth) identity tests
    pred: bool, // pixel activity vs residual energy
    dir: bool,  // directional coherence
    stat: bool, // fast-vs-slow residual energy change
    div4: bool, // ablation: leaf = number of distinct neighbour values (1..4) only
}
const FULL: TreeCfg = TreeCfg { flat: true, pred: true, dir: true, stat: true, div4: false };

#[inline(always)]
fn bl(v: u32) -> u32 {
    32 - v.leading_zeros()
}

/// Leaf id 0..=11 from causal quantities only.
#[inline(always)]
fn classify(t: &TreeCfg, w: i32, n: i32, nw: i32, ne: i32, s_h: u32, s_v: u32, s_d: u32, r: u32, e_slow: i32) -> usize {
    let g = s_h + s_v;
    if t.div4 {
        return (n != w) as usize + (nw != w && nw != n) as usize + (ne != w && ne != n && ne != nw) as usize;
    }
    if t.flat {
        let d = 1 + (n != w) as u32 + (nw != w && nw != n) as u32 + (ne != w && ne != n && ne != nw) as u32;
        if d == 1 {
            return (r > 0) as usize; // 0 flat & clean, 1 flat & active residual
        }
        if d == 2 {
            return 2 + (r > 0) as usize; // 2 two-level edge predicted, 3 two-level edge unpredicted
        }
    }
    if t.stat && bl(r * 10 >> 6).abs_diff(bl((e_slow >> 4) as u32)) >= 2 {
        return 4; // abrupt change vs long-run behaviour
    }
    if t.flat && g < 16 {
        return 5; // smooth intensity change
    }
    let p = if t.pred { if 4 * r < g { 0 } else if r >= g { 2 } else { 1 } } else { 1 };
    let dc = if t.dir {
        let m = s_h.min(s_v).min(s_d);
        let med = s_h + s_v + s_d - m - s_h.max(s_v).max(s_d);
        (med > 0 && 2 * m <= med) as usize
    } else {
        0
    };
    6 + p * 2 + dc
}

#[derive(Clone, Copy)]
struct Cfg {
    name: &'static str,
    tree: TreeCfg,
    compute: bool, // compute the leaf at all
    t: bool,       // leaf x qS context as a mixed input in Stage 3
    m: bool,       // mixer weight set chosen by leaf
    w: bool,       // one Shrub state per leaf
}
impl Cfg {
    const fn off(name: &'static str) -> Self {
        Cfg { name, tree: FULL, compute: false, t: false, m: false, w: false }
    }
    fn on(&self) -> bool {
        self.t || self.m || self.w
    }
}

// ------------------------------------------------------------------------------------------------ Stage 1 scan (copy of production + hook)

#[inline(always)]
fn paeth(a: i32, b: i32, c: i32) -> i32 {
    let p = a + b - c;
    let (pa, pb, pc) = ((p - a).abs(), (p - b).abs(), (p - c).abs());
    if pa <= pb && pa <= pc { a } else if pb <= pc { b } else { c }
}

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
    am_p: Vec<u32>, // |residual| rows, index x+1 (mirror of Stage 3's neighbour rows)
    am_c: Vec<u32>,
    hist: [u64; NCLS],
}

impl Scan {
    fn new(w: usize, h: usize, bits: u32) -> Self {
        Scan { w, h, bits, m: (1 << bits) - 1, ra: vec![0; w + 4], rb: vec![0; w + 4], rc: vec![0; w + 4], da: vec![[0; 4]; w + 1], db: vec![[0; 4]; w + 1], am_p: vec![0; w + 2], am_c: vec![0; w + 2], hist: [0; NCLS] }
    }

    /// `resolve(index, prediction, leaf) -> sample`. Everything else is shared by encoder and decoder.
    #[inline(always)]
    fn scan<F: FnMut(usize, u32, usize) -> u32>(&mut self, cfg: &Cfg, mut resolve: F) {
        let (w, h, m) = (self.w, self.h, self.m as i32);
        let mid = 1u32 << (self.bits - 1);
        let mut shrubs = [Shrub { e: [0; 3] }; NCLS];
        let mut e_slow = 0i32;
        let (mut cur, mut p, mut pp) = (&mut self.ra[..], &mut self.rb[..], &mut self.rc[..]);
        let (mut dcur, mut dprev) = (&mut self.da[..], &mut self.db[..]);
        let (mut am_c, mut am_p) = (&mut self.am_c[..], &mut self.am_p[..]);

        // Row 0 (leaf 0, no structure yet).
        am_c[0] = am_p[1];
        let x0 = resolve(0, mid, 0) as u16;
        cur[2] = x0;
        cur[0] = x0;
        cur[1] = x0;
        let a0 = (x0 as i32 - mid as i32).unsigned_abs();
        am_c[1] = a0;
        e_slow += (((a0 as i32) << 4) - e_slow) >> 4;
        for x in 1..w {
            let wv = cur[x + 1] as i32;
            let wwv = cur[x] as i32;
            let pr = ((5 * wv - wwv + 2) >> 2).clamp(0, m);
            let v = resolve(x, pr as u32, 0);
            cur[x + 2] = v as u16;
            let a = (v as i32 - pr).unsigned_abs();
            am_c[x + 1] = a;
            e_slow += (((a as i32) << 4) - e_slow) >> 4;
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
                let cls = if cfg.compute {
                    let mn = s45.min(s135);
                    let s_d = mn + (mn >> 2);
                    let j = x + 1;
                    let r = 2 * am_c[j - 1] + 2 * am_p[j] + am_p[j - 1] + am_p[j + 1];
                    classify(&cfg.tree, wv, n, nw, ne, s_h, s_v, s_d, r, e_slow)
                } else {
                    0
                };
                self.hist[cls] += 1;
                let sidx = if cfg.w { cls } else { 0 };
                let (pr, est) = predict(m, wv, wwv, n, nn, nw, ne, s_h, s_v, s45, s135, shrubs[sidx].factors());
                let xv = resolve(base + x, pr as u32, cls) as i32;
                shrubs[sidx].update(xv, est, m);
                let a = (xv - pr).unsigned_abs();
                am_c[x + 1] = a;
                e_slow += (((a as i32) << 4) - e_slow) >> 4;
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

// ------------------------------------------------------------------------------------------------ Stage 3 (copy of the BC path + tree inputs)

const S3_LIMIT_BC: u32 = 255;
const S3_MIX_SHIFT_BC: u32 = 11;
const S3_W_CLAMP: i32 = 1 << 20;
const S3_IN: usize = 5;
const NQ: usize = 32;
const ND: usize = 6;
const NW: usize = 8;
const OFF_B: usize = NQ * 64;
const OFF_C: usize = OFF_B + NQ * ND * 64;
const OFF_D: usize = OFF_C + NQ * NW * 64;
const OFF_T: usize = OFF_D + 64;
const SLOTS: usize = OFF_T + NCLS * NQ * 64;
const SLOT_INIT: u32 = 1 << 31;
const CONF: [i32; 5] = [0, 4, 6, 7, 8];

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
static SQUASH: [u16; 4096] = {
    let mut t = [0u16; 4096];
    let mut i = 0;
    while i < 4096 {
        t[i] = squash_fn(i as i32 - 2048) as u16;
        i += 1;
    }
    t
};
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

#[inline(always)]
fn s3q(v: u32) -> usize {
    if v < 4 {
        v as usize
    } else {
        let b = bl(v);
        (2 * b + ((v >> (b - 2)) & 1) - 2) as usize
    }
}
#[inline(always)]
fn upd(s: &mut u32, bit: u32) {
    let n = *s & 1023;
    let p = (*s >> 10) as i64;
    let target = if bit != 0 { (1i64 << 22) - 1 } else { 0 };
    let p = p + (((target - p) * RECIP[(n & 255) as usize] as i64) >> 16);
    *s = ((p as u32) << 10) | (n + (n < S3_LIMIT_BC) as u32);
}
#[inline(always)]
fn inp_of(s: u32) -> i32 {
    (STRETCH[(s >> 20) as usize] as i32 * CONF[((s & 1023) as usize).min(4)]) >> 3
}

struct S3 {
    w: usize,
    kmax: u32,
    x: usize,
    e: i32,
    prev_m: Vec<u32>,
    cur_m: Vec<u32>,
    prev_s: Vec<u8>,
    cur_s: Vec<u8>,
    t: Vec<u32>,
    sg: [u32; 9],
    wt: Vec<[i32; S3_IN]>,
    use_t: bool,
    mix_cls: bool,
}

impl S3 {
    fn new(w: usize, bits: u32, cfg: &Cfg) -> Self {
        let rmax = 2 * ((1u32 << bits) - 1);
        S3 {
            w,
            kmax: bl(rmax),
            x: 0,
            e: 0,
            prev_m: vec![0; w + 2],
            cur_m: vec![0; w + 2],
            prev_s: vec![1; w + 2],
            cur_s: vec![1; w + 2],
            t: vec![SLOT_INIT; SLOTS],
            sg: [SLOT_INIT; 9],
            wt: vec![[20000, 20000, 20000, 20000, 0]; NCLS * 64],
            use_t: cfg.t,
            mix_cls: cfg.m,
        }
    }

    #[inline(always)]
    fn bit<I: BitIo>(&mut self, io: &mut I, idx: [usize; 3], wb: usize, node: usize, want: u32) -> u32 {
        let (ib, ic, it) = (idx[0] + node, idx[1] + node, idx[2] + node);
        let inp = [if self.use_t { inp_of(self.t[it]) } else { 0 }, inp_of(self.t[ib]), inp_of(self.t[ic]), 0, 256];
        let ws = &mut self.wt[wb + node];
        let dot: i64 = (0..S3_IN).map(|i| (inp[i] * ws[i]) as i64).sum();
        let x = (dot >> 16).clamp(-2047, 2047) as i32;
        let p12 = SQUASH[(x + 2048) as usize] as i32;
        let p16 = (p12 * 16 + 8).clamp(32, 65535 - 32);
        let bit = io.code(p16 as u32, want);
        let err = ((bit as i32) << 12) - p12;
        for i in 0..S3_IN {
            ws[i] = (ws[i] + ((inp[i] * err) >> S3_MIX_SHIFT_BC)).clamp(-S3_W_CLAMP, S3_W_CLAMP);
        }
        upd(&mut self.t[ib], bit);
        upd(&mut self.t[ic], bit);
        if self.use_t {
            upd(&mut self.t[it], bit);
        }
        bit
    }

    #[inline(always)]
    fn code<I: BitIo>(&mut self, io: &mut I, r: i32, cls: usize) -> i32 {
        let x = self.x;
        let i = x + 1;
        if x == 0 {
            self.cur_m[0] = self.prev_m[1];
            self.cur_s[0] = 1;
        }
        let (aw, an, anw, ane) = (self.cur_m[i - 1], self.prev_m[i], self.prev_m[i - 1], self.prev_m[i + 1]);
        let qs = s3q(2 * aw + 2 * an + anw + ane).min(NQ - 1);
        let qd = s3q(aw.abs_diff(an)).min(ND - 1);
        let qe = s3q((self.e >> 2) as u32).min(NQ - 1);
        let qw = s3q(aw).min(NW - 1);
        let idx = [OFF_B + (qs * ND + qd) * 64, OFF_C + (qe * NW + qw) * 64, OFF_T + (cls * NQ + qs) * 64];
        let wb = if self.mix_cls { cls * 64 } else { (qs >> 3) * 64 };
        let sgi = self.cur_s[i - 1] as usize * 3 + self.prev_s[i] as usize;
        let a_in = r.unsigned_abs();
        let k_in = bl(a_in);
        let nz = self.bit(io, idx, wb, 0, (a_in != 0) as u32);
        let (mut a, mut neg) = (0u32, 0u32);
        if nz != 0 {
            let mut k = 1u32;
            while k < self.kmax {
                if self.bit(io, idx, wb, k as usize, (k_in > k) as u32) == 0 {
                    break;
                }
                k += 1;
            }
            a = 1;
            if k >= 2 {
                let t = self.bit(io, idx, wb, 15 + k as usize, (a_in >> (k - 2)) & 1);
                a = 2 + t;
                if k >= 3 {
                    let u = self.bit(io, idx, wb, 33 + 2 * (k as usize - 3) + t as usize, (a_in >> (k - 3)) & 1);
                    a = 2 * a + u;
                    for b in (0..k - 3).rev() {
                        a = 2 * a + io.code(32768, (a_in >> b) & 1);
                    }
                }
            }
            let ps = (self.sg[sgi] >> 16).clamp(32, 65535 - 32);
            neg = io.code(ps, (r < 0) as u32);
            upd(&mut self.sg[sgi], neg);
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

// ------------------------------------------------------------------------------------------------ plane / image codec

fn enc_plane(src: &[u16], w: usize, h: usize, bits: u32, cfg: &Cfg, enc: &mut Stage4Enc, hist: &mut [u64; NCLS]) -> Vec<u8> {
    enc.reset();
    let mut sc = Scan::new(w, h, bits);
    let mut s3 = S3::new(w, bits, cfg);
    sc.scan(cfg, |i, p, cls| {
        let x = src[i];
        s3.code(enc, x as i32 - p as i32, cls);
        x as u32
    });
    for k in 0..NCLS {
        hist[k] += sc.hist[k];
    }
    enc.finish().to_vec()
}

fn dec_plane(stream: &[u8], out: &mut [u16], w: usize, h: usize, bits: u32, cfg: &Cfg) -> bool {
    let mut dec = Stage4Dec::new(stream);
    let mut sc = Scan::new(w, h, bits);
    let mut s3 = S3::new(w, bits, cfg);
    let m = (1i32 << bits) - 1;
    let mut bad = false;
    sc.scan(cfg, |i, p, cls| {
        let v = p as i32 + s3.code(&mut dec, 0, cls);
        bad |= (v as u32) > m as u32;
        let v = v.clamp(0, m);
        out[i] = v as u16;
        v as u32
    });
    !bad
}

fn planes_of(img: &Canonical, ycocg: bool) -> (Vec<Vec<u16>>, Vec<u32>) {
    let ch = img.spec.layout.channels();
    let n = img.spec.width as usize * img.spec.height as usize;
    let mut planes = vec![vec![0u16; n]; ch];
    for (i, px) in img.data.chunks_exact(ch).enumerate() {
        for (k, &v) in px.iter().enumerate() {
            planes[k][i] = v as u16;
        }
    }
    let mut bits = vec![8u32; ch];
    if ycocg {
        assert!(ellie::ycocg_r_forward_planes(&mut planes[..3]));
        bits[1] = 9;
        bits[2] = 9;
    }
    (planes, bits)
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

fn center_crop(img: &Canonical, max: u32) -> Canonical {
    let (w, h) = (img.spec.width, img.spec.height);
    if w <= max && h <= max {
        return img.clone();
    }
    let (cw, ch) = (w.min(max), h.min(max));
    let (x0, y0) = ((w - cw) / 2, (h - ch) / 2);
    let row = img.spec.layout.channels() * img.spec.bytes_per_sample();
    let mut data = Vec::with_capacity(cw as usize * ch as usize * row);
    for y in y0..y0 + ch {
        let s = (y as usize * w as usize + x0 as usize) * row;
        data.extend_from_slice(&img.data[s..s + cw as usize * row]);
    }
    Canonical::new(Spec { width: cw, height: ch, ..img.spec }, data).unwrap()
}

fn best<T>(reps: u32, mut f: impl FnMut() -> T) -> (T, f64) {
    let (mut keep, mut t_best) = (None, f64::MAX);
    for _ in 0..reps {
        let t = Instant::now();
        let r = f();
        t_best = t_best.min(t.elapsed().as_secs_f64() * 1e3);
        keep = Some(r);
    }
    (keep.unwrap(), t_best)
}

fn configs() -> Vec<Cfg> {
    let on = |name, tree, t, m, w| Cfg { name, tree, compute: true, t, m, w };
    let abl = |f: fn(&mut TreeCfg)| {
        let mut t = FULL;
        f(&mut t);
        t
    };
    vec![
        Cfg::off("ctrl"),
        Cfg { name: "overhead", compute: true, ..Cfg::off("overhead") },
        Cfg { name: "dupT", t: true, ..Cfg::off("dupT") },
        on("T", FULL, true, false, false),
        on("M", FULL, false, true, false),
        on("W", FULL, false, false, true),
        on("TM", FULL, true, true, false),
        on("TW", FULL, true, false, true),
        on("TMW", FULL, true, true, true),
        on("T-noflat", abl(|t| t.flat = false), true, false, false),
        on("T-nopred", abl(|t| t.pred = false), true, false, false),
        on("T-nodir", abl(|t| t.dir = false), true, false, false),
        on("T-nostat", abl(|t| t.stat = false), true, false, false),
        on("T-div4", TreeCfg { div4: true, ..FULL }, true, false, false),
        on("TM-nostat", abl(|t| t.stat = false), true, true, false),
        on("T-flatonly", TreeCfg { flat: true, pred: false, dir: false, stat: false, div4: false }, true, false, false),
    ]
}

struct Row {
    image: String,
    cfg: &'static str,
    stream_bytes: usize,
    total_bytes: usize,
    enc_ms: f64,
    dec_ms: f64,
    exact: bool,
    det: bool,
    hist: [u64; NCLS],
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let arg = |k: &str| args.iter().position(|a| a == k).and_then(|i| args.get(i + 1)).cloned();
    let out = PathBuf::from(arg("--out").unwrap_or("benchmarks/ellie-structural-tree/results".into()));
    let reps: u32 = arg("--reps").map(|s| s.parse().unwrap()).unwrap_or(2);
    std::fs::create_dir_all(&out).unwrap();
    let cfgs = configs();
    let mut rows: Vec<Row> = vec![];
    let mut base_rows: Vec<(String, usize, f64, f64, f64, f64)> = vec![]; // image, prod bytes, prod enc(auto), enc(single), dec
    let mut all_ok = true;

    for f in ["portrait_0.jpg", "screen_0.png", "night_0.jpg"] {
        let name = f.split('.').next().unwrap().to_string();
        let img = center_crop(&Canonical::load(&Path::new("data").join(f)).unwrap(), CROP);
        let (w, h) = (img.spec.width as usize, img.spec.height as usize);
        // Unchanged production baseline.
        let (prod, enc_auto_ms) = best(reps, || pkon::encode_standard(&img).unwrap());
        assert_eq!(pkon::encode_standard(&img).unwrap(), prod, "production nondeterministic");
        let hdr = pkon::parse_header(&prod).unwrap();
        let CodecParams::Ellie(ep) = &hdr.params else { panic!() };
        let repr = ep.representation;
        let ycocg = repr == ELLIE_REPR_YCOCG_R;
        let single = if ycocg { EllieRepr::YCoCgR } else { EllieRepr::Untransformed };
        let (_, enc_single_ms) = best(reps, || pkon::encode_standard_with(&img, single).unwrap());
        let (dec, dec_ms) = best(reps, || pkon::decode(&prod).unwrap());
        assert!(matches!(dec, pkon::Decoded::Standard(ref c) if c.data == img.data), "production roundtrip");
        eprintln!("[{name}] production {} B, ycocg={ycocg}, enc(auto) {enc_auto_ms:.0} ms, enc(single) {enc_single_ms:.0} ms, dec {dec_ms:.0} ms", prod.len());
        base_rows.push((name.clone(), prod.len(), enc_auto_ms, enc_single_ms, dec_ms, 0.0));

        let (planes, bits) = planes_of(&img, ycocg);
        let payload = &prod[prod.len() - hdr.payload_len as usize..];
        let overhead = prod.len() - payload.len();
        let mut enc = Stage4Enc::with_capacity(w * h);
        for cfg in &cfgs {
            let ((streams, hist), enc_ms) = best(reps, || {
                let mut hist = [0u64; NCLS];
                let s: Vec<Vec<u8>> = planes.iter().zip(&bits).map(|(p, &b)| enc_plane(p, w, h, b, cfg, &mut enc, &mut hist)).collect();
                (s, hist)
            });
            let (streams2, _) = {
                let mut hist = [0u64; NCLS];
                (planes.iter().zip(&bits).map(|(p, &b)| enc_plane(p, w, h, b, cfg, &mut enc, &mut hist)).collect::<Vec<_>>(), 0)
            };
            let det = streams == streams2;
            let ((rec, ok), dec_ms) = best(reps, || {
                let mut out: Vec<Vec<u16>> = vec![vec![0u16; w * h]; planes.len()];
                let mut ok = true;
                for ((s, o), &b) in streams.iter().zip(out.iter_mut()).zip(&bits) {
                    ok &= dec_plane(s, o, w, h, b, cfg);
                }
                (pack(out, ycocg, w * h), ok)
            });
            let exact = ok && rec.as_deref() == Some(&img.data[..]);
            let stream_bytes: usize = streams.iter().map(|s| s.len()).sum();
            if !cfg.on() && !cfg.t {
                // tree-off configs must reproduce the production payload bytes exactly
                if cfg.name == "ctrl" || cfg.name == "overhead" {
                    assert_eq!(streams.concat(), payload, "{name}/{}: payload differs from production", cfg.name);
                }
            }
            let total = overhead + stream_bytes + cfg.on() as usize; // +1 mode byte whenever the tree influences coding
            eprintln!("  {:<11} {:>8} B  enc {:>6.0} ms dec {:>6.0} ms exact={exact} det={det}", cfg.name, total, enc_ms, dec_ms);
            all_ok &= exact && det;
            rows.push(Row { image: name.clone(), cfg: cfg.name, stream_bytes, total_bytes: total, enc_ms, dec_ms, exact, det, hist });
        }
    }

    // Report tables.
    let mut csv = String::from("image,config,stream_bytes,total_bytes,enc_ms,dec_ms,exact,deterministic\n");
    for r in &rows {
        csv += &format!("{},{},{},{},{:.1},{:.1},{},{}\n", r.image, r.cfg, r.stream_bytes, r.total_bytes, r.enc_ms, r.dec_ms, r.exact, r.det);
    }
    std::fs::write(out.join("per_image.csv"), csv).unwrap();
    let prod_total: usize = base_rows.iter().map(|b| b.1).sum();
    let mut md = format!("production baseline total: {prod_total} B ({})\n\n| config | total B | vs prod | portrait | screen | night | enc ms (sum) | dec ms (sum) |\n|---|---|---|---|---|---|---|---|\n", base_rows.iter().map(|b| format!("{} {}", b.0, b.1)).collect::<Vec<_>>().join(", "));
    for c in &cfgs {
        let rs: Vec<&Row> = rows.iter().filter(|r| r.cfg == c.name).collect();
        let tot: usize = rs.iter().map(|r| r.total_bytes).sum();
        let per: Vec<String> = rs.iter().zip(&base_rows).map(|(r, b)| format!("{:+.2}%", (r.total_bytes as f64 / b.1 as f64 - 1.0) * 100.0)).collect();
        md += &format!("| {} | {} | {:+.3}% | {} | {} | {} | {:.0} | {:.0} |\n", c.name, tot, (tot as f64 / prod_total as f64 - 1.0) * 100.0, per[0], per[1], per[2], rs.iter().map(|r| r.enc_ms).sum::<f64>(), rs.iter().map(|r| r.dec_ms).sum::<f64>());
    }
    md += "\nproduction timing (best of reps, ms): ";
    for b in &base_rows {
        md += &format!("{}: enc(auto,2 repr) {:.0}, enc(1 repr) {:.0}, dec {:.0}; ", b.0, b.2, b.3, b.4);
    }
    md += "\n\nleaf histogram (% of pixels, config T, all planes):\n\n| image | ";
    md += &(0..12).map(|i| i.to_string()).collect::<Vec<_>>().join(" | ");
    md += " |\n|---|";
    md += &"---|".repeat(12);
    md += "\n";
    for r in rows.iter().filter(|r| r.cfg == "T") {
        let tot: u64 = r.hist.iter().sum();
        md += &format!("| {} | {} |\n", r.image, (0..12).map(|i| format!("{:.1}", r.hist[i] as f64 * 100.0 / tot as f64)).collect::<Vec<_>>().join(" | "));
    }
    md += &format!("\nall exact+deterministic: {all_ok}\n");
    std::fs::write(out.join("summary.md"), &md).unwrap();
    println!("{md}");
    assert!(all_ok);
}
