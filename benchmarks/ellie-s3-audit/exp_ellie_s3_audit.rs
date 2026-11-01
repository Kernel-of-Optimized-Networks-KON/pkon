//! ELLIE Stage 3 probability-model audit (profile Bc255S11, three-image canary, 1024-px centre crops). Experiment only.
//!
//! Contains a private copy of the production Stage 3 `Bc255S11` path (contexts B and C only) so that the model can be
//! instrumented and varied without touching `src/ellie.rs`. Stage 1, Stage 2 and Stage 4 are the production code. The copy is
//! checked to reproduce the recovered production payload byte for byte (`ident`) before any measurement is trusted.
//!
//!   cargo run --release --bin exp_ellie_s3_audit -- --mode audit --out <dir>
//!   cargo run --release --bin exp_ellie_s3_audit -- --mode variant --var <name> --out <dir> [--reps N]
use nanoscope::canon::{Canonical, Spec};
use nanoscope::ellie::{self, Stage1, Stage2, Stage4Dec, Stage4Enc};
use nanoscope::pkon::{self, CodecParams, ELLIE_REPR_YCOCG_R, FIXED_HEADER_LEN};
use nanoscope::rangecoder::BitIo;
use std::path::PathBuf;
use std::time::Instant;

// ------------------------------------------------------------------ verbatim constants / tables from src/ellie.rs
const S3_W_CLAMP: i32 = 1 << 20;
const S3_IN: usize = 5;
const NQ: usize = 32;
const ND: usize = 6;
const NW: usize = 8;
const OFF_A: usize = 0;
const OFF_B: usize = OFF_A + NQ * 64;
const OFF_C: usize = OFF_B + NQ * ND * 64;
const OFF_D: usize = OFF_C + NQ * NW * 64;
const S3_SLOTS: usize = OFF_D + 64;
const SLOT_INIT: u32 = 1 << 31;
const CONF: [i32; 5] = [0, 4, 6, 7, 8];

const RECIP: [u32; 1024] = {
    let mut t = [0u32; 1024];
    let mut n = 0;
    while n < 1024 {
        t[n] = 131072 / (2 * n as u32 + 3);
        n += 1;
    }
    t
};

const fn squash_fn(d: i32) -> i32 {
    const T: [i32; 33] = [
        1, 2, 3, 6, 10, 16, 27, 45, 73, 120, 194, 310, 488, 747, 1101, 1546, 2047, 2549, 2994, 3348, 3607, 3785, 3901, 3975, 4022, 4050, 4068, 4079, 4085, 4089, 4092, 4093, 4094,
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
        let bl = 32 - v.leading_zeros();
        (2 * bl + ((v >> (bl - 2)) & 1) - 2) as usize
    }
}
/// Finer quantiser used only by the diagnostics: identity below 8, then four levels per octave.
#[inline(always)]
fn s3q4(v: u32) -> usize {
    if v < 8 {
        v as usize
    } else {
        let bl = 32 - v.leading_zeros();
        (4 * (bl - 4) + 8 + ((v >> (bl - 3)) & 3)) as usize
    }
}

#[inline(always)]
fn upd(s: &mut u32, bit: u32, limit: u32) {
    let n = *s & 1023;
    let p = (*s >> 10) as i64;
    let target = if bit != 0 { (1i64 << 22) - 1 } else { 0 };
    let p = p + (((target - p) * RECIP[n as usize] as i64) >> 16);
    *s = ((p as u32) << 10) | (n + (n < limit) as u32);
}
#[inline(always)]
fn inp(s: u32) -> i32 {
    let n = ((s & 1023) as usize).min(4);
    (STRETCH[(s >> 20) as usize] as i32 * CONF[n]) >> 3
}

// ------------------------------------------------------------------ experiment configuration
/// Experimental switches. All default to off (= production Bc255S11). Each keeps the number of adaptive slots unchanged.
#[derive(Clone, Copy, Debug)]
struct Cfg {
    limit: u32,
    shift: u32,
    /// Zero flag only: C's group index also carries "N != 0" (uses qe groups 16..31, which the clamp qe <= 15 frees).
    znz: bool,
    /// C = 16 qe levels x 16 qw levels (same 256 groups): extends the qw ladder from aw < 12 to aw < ~200.
    c16: bool,
    /// B = 18 qs levels x 10 qd levels (180 <= 192 groups): extends the qd ladder from diff < 6 to diff < ~24.
    bqd10: bool,
}
const BASE: Cfg = Cfg { limit: 255, shift: 11, znz: false, c16: false, bqd10: false };

// ------------------------------------------------------------------ diagnostics
const NP: usize = 10;
const PNAMES: [&str; NP] = ["B", "C", "B_qs4", "B_qd12", "C_qe4", "C_qw12", "B+cat", "C+cat", "B+Nz", "C+Nz"];
const TAB_SZ: usize = 64 * 4096;
const WINS: [u32; 2] = [256, 1024];

struct Stats {
    tab: Vec<f64>,
    pix: u64,
    npix: u64,
    // zero-flag categories (W!=0)*2+(N!=0): 0: W=0,N=0  1: W=0,N!=0  2: W!=0,N=0  3: W!=0,N!=0
    zc_n: [u64; 4],
    zc_z: [u64; 4],
    zc_cost: [f64; 4],
    cnt: Vec<Vec<[u32; 2]>>,
    cnt2: Vec<Vec<[u32; 2]>>, // second raster half only, partitions B and C
    bcost: Vec<f64>,
    ccost: Vec<f64>,
    bfeat: Vec<[f64; 3]>, // per B key: n, sum(sum), sum(diff)
    cfeat: Vec<[f64; 3]>, // per C key: n, sum(e>>2), sum(aw)
    class_cost: [f64; 3],
    // sliding-window deviation of the slot probabilities from the empirical window frequency
    wn: [Vec<u32>; 2],
    wo: [Vec<u32>; 2],
    wp: [Vec<f64>; 2],
    wdev: [[f64; 2]; 2],  // [window][class 0/1] sum of n*KL bits
    wcount: [[u64; 2]; 2],
    px_cost: Vec<f32>,
    px_cost0: Vec<f32>,
    cur_cost: f32,
    cur_cost0: f32,
    // per-sample keys
    pk: [usize; NP],
    cat: usize,
}

impl Stats {
    fn new(npix: usize) -> Self {
        Self {
            tab: (0..=65536usize).map(|p| if p == 0 { f64::INFINITY } else { -(p as f64 / 65536.0).log2() }).collect(),
            pix: 0,
            npix: npix as u64,
            zc_n: [0; 4],
            zc_z: [0; 4],
            zc_cost: [0.0; 4],
            cnt: vec![vec![[0; 2]; TAB_SZ]; NP],
            cnt2: vec![vec![[0; 2]; TAB_SZ]; 2],
            bcost: vec![0.0; TAB_SZ],
            ccost: vec![0.0; TAB_SZ],
            bfeat: vec![[0.0; 3]; 4096],
            cfeat: vec![[0.0; 3]; 4096],
            class_cost: [0.0; 3],
            wn: [vec![0; S3_SLOTS], vec![0; S3_SLOTS]],
            wo: [vec![0; S3_SLOTS], vec![0; S3_SLOTS]],
            wp: [vec![0.0; S3_SLOTS], vec![0.0; S3_SLOTS]],
            wdev: [[0.0; 2]; 2],
            wcount: [[0; 2]; 2],
            px_cost: Vec::with_capacity(npix),
            px_cost0: Vec::with_capacity(npix),
            cur_cost: 0.0,
            cur_cost0: 0.0,
            pk: [0; NP],
            cat: 0,
        }
    }
    fn win(&mut self, slot: usize, p: f64, bit: u32) {
        let node = slot % 64;
        if node > 16 {
            return;
        }
        let cl = (node > 0) as usize;
        for wi in 0..2 {
            self.wn[wi][slot] += 1;
            self.wo[wi][slot] += bit;
            self.wp[wi][slot] += p;
            if self.wn[wi][slot] == WINS[wi] {
                let n = WINS[wi] as f64;
                let f = (self.wo[wi][slot] as f64 / n).clamp(1e-6, 1.0 - 1e-6);
                let pb = (self.wp[wi][slot] / n).clamp(1e-4, 1.0 - 1e-4);
                let kl = f * (f / pb).log2() + (1.0 - f) * ((1.0 - f) / (1.0 - pb)).log2();
                self.wdev[wi][cl] += n * kl;
                self.wcount[wi][cl] += 1;
                self.wn[wi][slot] = 0;
                self.wo[wi][slot] = 0;
                self.wp[wi][slot] = 0.0;
            }
        }
    }
    fn record(&mut self, node: usize, bit: u32, p16: u32, slot_b: u32, slot_c: u32, ib: usize, ic: usize) {
        let cost = self.tab[if bit != 0 { p16 } else { 65536 - p16 } as usize];
        let cls = if node == 0 { 0 } else if node <= 16 { 1 } else { 2 };
        self.class_cost[cls] += cost;
        self.cur_cost += cost as f32;
        if node == 0 {
            self.cur_cost0 += cost as f32;
            self.zc_n[self.cat] += 1;
            self.zc_z[self.cat] += (bit == 0) as u64;
            self.zc_cost[self.cat] += cost;
        }
        let second = self.pix >= self.npix / 2;
        for p in 0..NP {
            if p >= 6 && node != 0 {
                continue;
            }
            let e = &mut self.cnt[p][node * 4096 + self.pk[p]];
            e[bit as usize] += 1;
            if second && p < 2 {
                self.cnt2[p][node * 4096 + self.pk[p]][bit as usize] += 1;
            }
        }
        self.bcost[node * 4096 + self.pk[0]] += cost;
        self.ccost[node * 4096 + self.pk[1]] += cost;
        self.win(ib, (slot_b >> 10) as f64 / (1u64 << 22) as f64, bit);
        self.win(ic, (slot_c >> 10) as f64 / (1u64 << 22) as f64, bit);
    }
}

fn lgamma(x: f64) -> f64 {
    const G: [f64; 9] = [
        0.99999999999980993, 676.5203681218851, -1259.1392167224028, 771.32342877765313, -176.61502916214059, 12.507343278686905, -0.13857109526572012, 9.9843695780195716e-6,
        1.5056327351493116e-7,
    ];
    let x = x - 1.0;
    let mut a = G[0];
    let t = x + 7.5;
    for (i, g) in G.iter().enumerate().skip(1) {
        a += g / (x + i as f64);
    }
    0.5 * (2.0 * std::f64::consts::PI).ln() + (x + 0.5) * t.ln() - t + a.ln()
}
/// Order-independent adaptive (Krichevsky-Trofimov) code length of a binary sequence with counts (a, b), in bits.
fn kt(c: [u32; 2]) -> f64 {
    let (a, b) = (c[0] as f64, c[1] as f64);
    -(lgamma(a + 0.5) + lgamma(b + 0.5) - std::f64::consts::PI.ln() - lgamma(a + b + 1.0)) / std::f64::consts::LN_2
}
fn hbin(c: [u32; 2]) -> f64 {
    let n = (c[0] + c[1]) as f64;
    if n == 0.0 || c[0] == 0 || c[1] == 0 {
        return 0.0;
    }
    let p = c[1] as f64 / n;
    n * -(p * p.log2() + (1.0 - p) * (1.0 - p).log2())
}

// ------------------------------------------------------------------ experimental Stage 3 (copy of the Bc255S11 path)
struct M {
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
    cfg: Cfg,
    st: Option<Box<Stats>>,
}

impl M {
    fn new(w: usize, h: usize, bits: u32, cfg: Cfg, stats: bool) -> Self {
        let rmax = 2 * ((1u32 << bits) - 1);
        Self {
            w,
            kmax: 32 - rmax.leading_zeros(),
            x: 0,
            e: 0,
            prev_m: vec![0; w + 2],
            cur_m: vec![0; w + 2],
            prev_s: vec![1; w + 2],
            cur_s: vec![1; w + 2],
            t: vec![SLOT_INIT; S3_SLOTS],
            sg: [SLOT_INIT; 9],
            wt: vec![[20000, 20000, 20000, 20000, 0]; 4 * 64],
            cfg,
            st: if stats { Some(Box::new(Stats::new(w * h))) } else { None },
        }
    }

    #[inline(always)]
    fn code_bit<I: BitIo, const ST: bool>(&mut self, io: &mut I, idx: [usize; 3], wb: usize, node: usize, want: u32) -> u32 {
        let (ib, ic) = (idx[0] + node, if node == 0 { idx[2] } else { idx[1] + node });
        let (limit, shift) = (self.cfg.limit, self.cfg.shift);
        let (sb, sc) = (self.t[ib], self.t[ic]);
        let input = [0, inp(sb), inp(sc), 0, 256];
        let ws = &mut self.wt[wb + node];
        let dot = (input[1] * ws[1]) as i64 + (input[2] * ws[2]) as i64 + (input[4] * ws[4]) as i64;
        let x = (dot >> 16).clamp(-2047, 2047) as i32;
        let p12 = SQUASH[(x + 2048) as usize] as i32;
        let p16 = (p12 * 16 + 8).clamp(32, 65535 - 32);
        let bit = io.code(p16 as u32, want);
        let err = ((bit as i32) << 12) - p12;
        for i in 0..S3_IN {
            ws[i] = (ws[i] + ((input[i] * err) >> shift)).clamp(-S3_W_CLAMP, S3_W_CLAMP);
        }
        if ST {
            self.st.as_mut().unwrap().record(node, bit, p16 as u32, sb, sc, ib, ic);
        }
        upd(&mut self.t[ib], bit, limit);
        upd(&mut self.t[ic], bit, limit);
        bit
    }

    #[inline(always)]
    fn code_sample<I: BitIo, const ST: bool>(&mut self, io: &mut I, r: i32) -> i32 {
        let x = self.x;
        let i = x + 1;
        if x == 0 {
            self.cur_m[0] = self.prev_m[1];
            self.cur_s[0] = 1;
        }
        let (aw, an, anw, ane) = (self.cur_m[i - 1], self.prev_m[i], self.prev_m[i - 1], self.prev_m[i + 1]);
        let sum = 2 * aw + 2 * an + anw + ane;
        let qs_full = s3q(sum).min(NQ - 1);
        let (qs, qd_lv) = if self.cfg.bqd10 { (qs_full.min(17), 10) } else { (qs_full, ND) };
        let qd = s3q(aw.abs_diff(an)).min(qd_lv - 1);
        let e4 = (self.e >> 2) as u32;
        let (qe, qw, nw) = if self.cfg.c16 { (s3q(e4).min(15), s3q(aw).min(15), 16) } else if self.cfg.znz { (s3q(e4).min(15), s3q(aw).min(NW - 1), NW) } else { (s3q(e4).min(NQ - 1), s3q(aw).min(NW - 1), NW) };
        let c = OFF_C + (qe * nw + qw) * 64;
        let c0 = if self.cfg.znz { OFF_C + ((qe + 16 * (an != 0) as usize) * NW + qw) * 64 } else { c };
        let idx = [OFF_B + (qs * qd_lv + qd) * 64, c, c0];
        let wb = (qs_full >> 3) * 64;
        let sgi = self.cur_s[i - 1] as usize * 3 + self.prev_s[i] as usize;
        if ST {
            let st = self.st.as_mut().unwrap();
            let cat = (aw != 0) as usize * 2 + (an != 0) as usize;
            let nz = (an != 0) as usize;
            let (bk, ck) = (qs * ND + qd, qe * NW + qw);
            let diff = aw.abs_diff(an);
            st.cat = cat;
            st.pk = [
                bk,
                ck,
                s3q4(sum).min(63) * ND + qd,
                qs * 12 + s3q(diff).min(11),
                s3q4(e4).min(63) * NW + qw,
                qe * 12 + s3q(aw).min(11),
                bk * 4 + cat,
                ck * 4 + cat,
                bk * 2 + nz,
                ck * 2 + nz,
            ];
            st.cur_cost = 0.0;
            st.cur_cost0 = 0.0;
            let f = &mut st.bfeat[bk];
            f[0] += 1.0;
            f[1] += sum as f64;
            f[2] += diff as f64;
            let f = &mut st.cfeat[ck];
            f[0] += 1.0;
            f[1] += e4 as f64;
            f[2] += aw as f64;
        }

        let a_in = r.unsigned_abs();
        let k_in = 32 - a_in.leading_zeros();
        let nzf = self.code_bit::<I, ST>(io, idx, wb, 0, (a_in != 0) as u32);
        let mut a = 0u32;
        let mut neg = 0u32;
        if nzf != 0 {
            let mut k = 1u32;
            while k < self.kmax {
                if self.code_bit::<I, ST>(io, idx, wb, k as usize, (k_in > k) as u32) == 0 {
                    break;
                }
                k += 1;
            }
            a = 1;
            if k >= 2 {
                let t = self.code_bit::<I, ST>(io, idx, wb, 15 + k as usize, (a_in >> (k - 2)) & 1);
                a = 2 + t;
                if k >= 3 {
                    let u = self.code_bit::<I, ST>(io, idx, wb, 33 + 2 * (k as usize - 3) + t as usize, (a_in >> (k - 3)) & 1);
                    a = 2 * a + u;
                    for b in (0..k - 3).rev() {
                        a = 2 * a + io.code(32768, (a_in >> b) & 1);
                    }
                }
            }
            let ps = (self.sg[sgi] >> 16).clamp(32, 65535 - 32);
            neg = io.code(ps, (r < 0) as u32);
            upd(&mut self.sg[sgi], neg, self.cfg.limit);
        }
        if ST {
            let st = self.st.as_mut().unwrap();
            st.px_cost.push(st.cur_cost);
            st.px_cost0.push(st.cur_cost0);
            st.pix += 1;
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

// ------------------------------------------------------------------ corpus plumbing (same conventions as the other ELLIE harnesses)
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
fn split_planes(img: &Canonical, ycocg: bool) -> Vec<Vec<u16>> {
    assert_eq!(img.spec.bit_depth, 8);
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

struct Img {
    name: String,
    w: usize,
    h: usize,
    ycocg: bool,
    bits: Vec<u8>,
    planes: Vec<Vec<u16>>,
    res: Vec<Vec<i32>>, // Stage 2 output (the exact Stage 3 input) per plane
    prod_payload: Vec<u8>,
    prod_file: usize,
}

fn load3(refdir: &PathBuf) -> Vec<Img> {
    let data = PathBuf::from("data");
    ["portrait_0.jpg", "screen_0.png", "night_0.jpg"]
        .iter()
        .map(|f| {
            let name = f.split('.').next().unwrap().to_string();
            let img = center_crop(&Canonical::load(&data.join(f)).unwrap(), 1024);
            let (w, h) = (img.spec.width as usize, img.spec.height as usize);
            let prod = std::fs::read(refdir.join(format!("{name}.pkon"))).unwrap();
            let hdr = pkon::parse_header(&prod).unwrap();
            let CodecParams::Ellie(p) = &hdr.params else { panic!("not ELLIE") };
            let ycocg = p.representation == ELLIE_REPR_YCOCG_R;
            let bits: Vec<u8> = p.planes.iter().map(|x| x.0).collect();
            let planes = split_planes(&img, ycocg);
            let res = planes
                .iter()
                .zip(&bits)
                .map(|(pl, &b)| {
                    let mut s1 = Stage1::new(w, h, b as u32).unwrap();
                    let mut r = vec![0i32; w * h];
                    s1.encode(pl, &mut r).unwrap();
                    Stage2::new().encode_in_place(&mut r);
                    r
                })
                .collect();
            let prod_payload = prod[FIXED_HEADER_LEN + hdr.meta_len as usize..].to_vec();
            Img { name, w, h, ycocg, bits, planes, res, prod_payload, prod_file: prod.len() }
        })
        .collect()
}

/// Encode all planes with the experimental model; per-plane Stage 4 streams concatenated exactly as production does.
fn encode_img(im: &Img, cfg: Cfg) -> Vec<Vec<u8>> {
    let mut enc = Stage4Enc::with_capacity(im.w * im.h);
    im.res
        .iter()
        .zip(&im.bits)
        .map(|(r, &b)| {
            let mut m = M::new(im.w, im.h, b as u32, cfg, false);
            enc.reset();
            for &v in r {
                m.code_sample::<_, false>(&mut enc, v);
            }
            enc.finish().to_vec()
        })
        .collect()
}

/// Full decode (Stage 4, experimental Stage 3, production Stage 2 and Stage 1, inverse colour transform) and compare to the source.
fn decode_img(im: &Img, cfg: Cfg, streams: &[Vec<u8>]) -> bool {
    let mut planes: Vec<Vec<u16>> = vec![];
    for (k, s) in streams.iter().enumerate() {
        let b = im.bits[k] as u32;
        let mut m = M::new(im.w, im.h, b, cfg, false);
        let mut dec = Stage4Dec::new(s);
        let mut res = vec![0i32; im.w * im.h];
        for o in res.iter_mut() {
            *o = m.code_sample::<_, false>(&mut dec, 0);
        }
        Stage2::new().decode_in_place(&mut res);
        let mut out = vec![0u16; im.w * im.h];
        Stage1::new(im.w, im.h, b).unwrap().decode(&res, &mut out).unwrap();
        planes.push(out);
    }
    if im.ycocg && !ellie::ycocg_r_inverse_planes(&mut planes[..3]) {
        return false;
    }
    let mut src = im.planes.clone();
    if im.ycocg {
        assert!(ellie::ycocg_r_inverse_planes(&mut src[..3]));
    }
    planes == src
}

// ------------------------------------------------------------------ audit
#[derive(Default)]
struct Trans {
    // [kind 0 steady,1 up,2 down][bin]: segs, sum seg cost/px, sum first-4px cost/px
    n: [[f64; 8]; 3],
    c: [[f64; 8]; 3],
    c4: [[f64; 8]; 3],
    z: [[f64; 8]; 3], // zero-flag part
}

const SEG: usize = 16;
fn abin(m: f64) -> usize {
    const T: [f64; 7] = [0.15, 0.5, 1.0, 2.0, 4.0, 8.0, 16.0];
    T.iter().position(|&t| m < t).unwrap_or(7)
}
fn transitions(st: &Stats, res: &[i32], w: usize, h: usize, t: &mut Trans) {
    let ns = w / SEG;
    for y in 0..h {
        let mut prev = None;
        for s in 0..ns {
            let r0 = y * w + s * SEG;
            let act: f64 = res[r0..r0 + SEG].iter().map(|v| v.unsigned_abs() as f64).sum::<f64>() / SEG as f64;
            let b = abin(act);
            if let Some(pb) = prev {
                let kind = if b == pb { 0 } else if b >= pb + 2 { 1 } else if pb >= b + 2 { 2 } else { 3 };
                if kind < 3 {
                    let cs: f64 = st.px_cost[r0..r0 + SEG].iter().map(|&v| v as f64).sum::<f64>() / SEG as f64;
                    let c4: f64 = st.px_cost[r0..r0 + 4].iter().map(|&v| v as f64).sum::<f64>() / 4.0;
                    let z: f64 = st.px_cost0[r0..r0 + SEG].iter().map(|&v| v as f64).sum::<f64>() / SEG as f64;
                    t.n[kind][b] += 1.0;
                    t.c[kind][b] += cs;
                    t.c4[kind][b] += c4;
                    t.z[kind][b] += z;
                }
            }
            prev = Some(b);
        }
    }
}

fn audit(imgs: &[Img], out: &PathBuf) {
    // Aggregate over all images/planes; each plane is an independent model (reset semantics), stats summed.
    let mut zc_n = [0u64; 4];
    let mut zc_z = [0u64; 4];
    let mut zc_cost = [0f64; 4];
    let mut cnt = vec![vec![[0u64; 2]; TAB_SZ]; NP];
    let mut kt_part = [[0f64; 3]; NP]; // per partition per class (zero, unary, mant) — summed per plane (each plane a fresh learner)
    let mut half_gain = [0f64; 2]; // KT(all) - (KT(h1)+KT(h2)), partitions B and C
    let mut class_cost = [0f64; 3];
    let mut bcost = vec![0f64; TAB_SZ];
    let mut ccost = vec![0f64; TAB_SZ];
    let mut bfeat = vec![[0f64; 3]; 4096];
    let mut cfeat = vec![[0f64; 3]; 4096];
    let mut wdev = [[0f64; 2]; 2];
    let mut wcount = [[0u64; 2]; 2];
    let mut tr = Trans::default();
    let mut total_px = 0u64;
    let mut payload_bytes = 0usize;
    for im in imgs {
        let mut streams = vec![];
        let mut enc = Stage4Enc::with_capacity(im.w * im.h);
        for (k, r) in im.res.iter().enumerate() {
            let mut m = M::new(im.w, im.h, im.bits[k] as u32, BASE, true);
            enc.reset();
            for &v in r {
                m.code_sample::<_, true>(&mut enc, v);
            }
            streams.push(enc.finish().to_vec());
            let st = m.st.take().unwrap();
            total_px += st.npix;
            for c in 0..4 {
                zc_n[c] += st.zc_n[c];
                zc_z[c] += st.zc_z[c];
                zc_cost[c] += st.zc_cost[c];
            }
            for c in 0..3 {
                class_cost[c] += st.class_cost[c];
            }
            for p in 0..NP {
                for (i, e) in st.cnt[p].iter().enumerate() {
                    if e[0] + e[1] == 0 {
                        continue;
                    }
                    let node = i / 4096;
                    let cl = if node == 0 { 0 } else if node <= 16 { 1 } else { 2 };
                    kt_part[p][cl] += kt(*e);
                    cnt[p][i][0] += e[0] as u64;
                    cnt[p][i][1] += e[1] as u64;
                }
            }
            for p in 0..2 {
                for (i, e2) in st.cnt2[p].iter().enumerate() {
                    let e = st.cnt[p][i];
                    if e[0] + e[1] == 0 {
                        continue;
                    }
                    let e1 = [e[0] - e2[0], e[1] - e2[1]];
                    half_gain[p] += kt(e) - kt(e1) - kt(*e2);
                }
            }
            for i in 0..TAB_SZ {
                bcost[i] += st.bcost[i];
                ccost[i] += st.ccost[i];
            }
            for i in 0..4096 {
                for j in 0..3 {
                    bfeat[i][j] += st.bfeat[i][j];
                    cfeat[i][j] += st.cfeat[i][j];
                }
            }
            for a in 0..2 {
                for b in 0..2 {
                    wdev[a][b] += st.wdev[a][b];
                    wcount[a][b] += st.wcount[a][b];
                }
            }
            transitions(&st, &im.res[k], im.w, im.h, &mut tr);
        }
        let ident = streams.concat() == im.prod_payload;
        payload_bytes += streams.iter().map(|s| s.len()).sum::<usize>();
        println!("{}: payload {} B, production payload {} B, byte-identical to production = {ident}", im.name, streams.iter().map(|s| s.len()).sum::<usize>(), im.prod_payload.len());
        assert!(ident, "experimental Stage 3 copy diverges from production");
    }
    let total_cost: f64 = class_cost.iter().sum::<f64>();
    println!("\npixels {total_px}  model cost (node decisions only, excl. raw mantissa + sign) {:.0} B   payload total {payload_bytes} B", total_cost / 8.0);
    println!("class cost bytes: zero {:.0}  unary {:.0}  mant_modelled {:.0}", class_cost[0] / 8.0, class_cost[1] / 8.0, class_cost[2] / 8.0);

    // ---- Step 2: zero-flag categories
    println!("\n== Step 2: zero-flag by west/north corrected-residual zero state ==");
    println!("{:<14}{:>11}{:>9}{:>10}{:>12}{:>13}{:>9}", "category", "n", "share%", "P(zero)", "H(static)b", "model cost b", "cost/H");
    let names = ["W=0,N=0", "W=0,N!=0", "W!=0,N=0", "W!=0,N!=0"];
    let mut csv = String::from("category,n,share_pct,p_zero,empirical_entropy_bits,model_cost_bits,model_over_entropy\n");
    let mut totn = 0u64;
    let mut toth = 0f64;
    for c in 0..4 {
        let n = zc_n[c];
        let h = {
            let z = zc_z[c] as f64;
            let o = (n - zc_z[c]) as f64;
            let nn = n as f64;
            if z == 0.0 || o == 0.0 { 0.0 } else { -(z * (z / nn).log2() + o * (o / nn).log2()) }
        };
        totn += n;
        toth += h;
        println!("{:<14}{:>11}{:>9.2}{:>10.4}{:>12.0}{:>13.0}{:>9.3}", names[c], n, n as f64 / total_px as f64 * 100.0, zc_z[c] as f64 / n as f64, h, zc_cost[c], zc_cost[c] / h);
        csv += &format!("{},{},{:.3},{:.5},{:.1},{:.1},{:.4}\n", names[c], n, n as f64 / total_px as f64 * 100.0, zc_z[c] as f64 / n as f64, h, zc_cost[c], zc_cost[c] / h);
    }
    let zc_total: f64 = zc_cost.iter().sum();
    let z_all: u64 = zc_z.iter().sum();
    let h_all = { let z = z_all as f64; let o = (totn - z_all) as f64; -(z * (z / totn as f64).log2() + o * (o / totn as f64).log2()) };
    println!("{:<14}{:>11}{:>9}{:>10.4}{:>12.0}{:>13.0}", "all", totn, "", z_all as f64 / totn as f64, h_all, zc_total);
    println!("sum of per-category static entropies {:.0} b; model zero-flag cost {:.0} b ({:.2}% of total model cost)", toth, zc_total, zc_total / total_cost * 100.0);
    std::fs::write(out.join("zero_categories.csv"), csv).unwrap();

    println!("\nAdaptive (KT) code length of the zero flag under each partition [bytes] — how much information each key carries:");
    for p in 0..NP {
        println!("  {:<8} zero-flag KT {:>10.0} B   unary KT {:>10.0} B   mant KT {:>10.0} B", PNAMES[p], kt_part[p][0] / 8.0, kt_part[p][1] / 8.0, kt_part[p][2] / 8.0);
    }
    println!("  (model's actual mixed cost: zero {:.0} B  unary {:.0} B  mant {:.0} B)", class_cost[0] / 8.0, class_cost[1] / 8.0, class_cost[2] / 8.0);
    // static purity: how different is P(zero) across categories inside the same B / C context?
    for (pi, nm) in [(0usize, "B"), (1, "C")] {
        let mut mixed = 0u64;
        let mut tot = 0u64;
        let mut max_spread = 0f64;
        // categories per context come from the +cat partitions: key = ctx*4 + cat
        let pc = if pi == 0 { 6 } else { 7 };
        let nctx = if pi == 0 { NQ * ND } else { NQ * NW };
        for ctx in 0..nctx {
            let mut q = [(0u64, 0f64); 4];
            for cat in 0..4 {
                let e = cnt[pc][ctx * 4 + cat];
                let n = e[0] + e[1];
                q[cat] = (n, if n > 0 { e[0] as f64 / n as f64 } else { 0.0 });
            }
            let live: Vec<_> = q.iter().filter(|x| x.0 >= 200).collect();
            let n: u64 = q.iter().map(|x| x.0).sum();
            tot += n;
            if live.len() >= 2 {
                let lo = live.iter().map(|x| x.1).fold(1.0, f64::min);
                let hi = live.iter().map(|x| x.1).fold(0.0, f64::max);
                if hi - lo > 0.10 {
                    mixed += n;
                }
                max_spread = max_spread.max(hi - lo);
            }
        }
        println!("  context {nm}: {:.2}% of zero-flag observations sit in contexts whose populated categories (>=200 obs) differ in P(zero) by >0.10 (max spread {:.3})", mixed as f64 / tot as f64 * 100.0, max_spread);
    }

    // ---- Step 3: adaptation
    println!("\n== Step 3: adaptation ==");
    for (wi, wn) in WINS.iter().enumerate() {
        for (cl, nm) in ["zero-flag slots", "unary slots"].iter().enumerate() {
            let k = wcount[wi][cl] as f64;
            println!(
                "  window {wn:>4} obs, {nm:<16}: {:>7} windows, mean n*KL(window freq || mean slot p) = {:.3} bits/window (ideal stationary floor 0.721); excess {:.1} B total = {:.3}% of {} cost",
                wcount[wi][cl], wdev[wi][cl] / k, (wdev[wi][cl] - 0.7213 * k) / 8.0, (wdev[wi][cl] - 0.7213 * k) / class_cost[cl] * 100.0, ["zero", "unary"][cl]
            );
        }
    }
    println!("  midpoint change test (KT(all) - KT(first half) - KT(second half), all nodes; negative = no evidence of a regime change): B {:.0} B, C {:.0} B", half_gain[0] / 8.0, half_gain[1] / 8.0);
    println!("\n  transition windows: cost per pixel in a {SEG}-px row segment whose previous segment had a different mean |r| bin");
    println!("  bin edges (mean |r|): <0.15,<0.5,<1,<2,<4,<8,<16,>=16");
    let mut csv = String::from("kind,bin,segments,seg_bits_per_px,first4_bits_per_px,zero_flag_bits_per_px\n");
    for (k, nm) in ["steady", "up", "down"].iter().enumerate() {
        for b in 0..8 {
            if tr.n[k][b] > 0.0 {
                csv += &format!("{nm},{b},{},{:.4},{:.4},{:.4}\n", tr.n[k][b], tr.c[k][b] / tr.n[k][b], tr.c4[k][b] / tr.n[k][b], tr.z[k][b] / tr.n[k][b]);
            }
        }
    }
    std::fs::write(out.join("transitions.csv"), csv).unwrap();
    for (k, nm) in [(1usize, "up  (>=2 bins higher)"), (2, "down (>=2 bins lower)")] {
        let (mut nt, mut ex, mut ex4, mut exz, mut base) = (0f64, 0f64, 0f64, 0f64, 0f64);
        for b in 0..8 {
            if tr.n[k][b] > 0.0 && tr.n[0][b] > 0.0 {
                let n = tr.n[k][b];
                nt += n;
                ex += tr.c[k][b] - n * tr.c[0][b] / tr.n[0][b];
                ex4 += tr.c4[k][b] - n * tr.c4[0][b] / tr.n[0][b];
                exz += tr.z[k][b] - n * tr.z[0][b] / tr.n[0][b];
                base += n * tr.c[0][b] / tr.n[0][b];
            }
        }
        println!("  {nm}: {:>8.0} segments; excess vs steady segments of the same bin: segment {:+.4} b/px ({:+.2}%), first-4px {:+.4} b/px, of which zero flag {:+.4} b/px", nt, ex / nt, ex / base * 100.0, ex4 / nt, exz / nt);
    }

    // ---- Step 4: contexts
    println!("\n== Step 4: context tables ==");
    let tabs = [("B", 0usize, NQ * ND, ND, &bcost, &bfeat), ("C", 1usize, NQ * NW, NW, &ccost, &cfeat)];
    for (nm, pi, nctx, stride, cost, feat) in tabs {
        let mut csv = String::from(if nm == "B" { "qs,qd,pixels,mean_sum,mean_absdiff,p_zero,H_zero_bits,cost_zero_bits,unary_decisions,H_unary_bits,cost_unary_bits,mant_decisions,H_mant_bits,cost_mant_bits\n" } else { "qe,qw,pixels,mean_e4,mean_aw,p_zero,H_zero_bits,cost_zero_bits,unary_decisions,H_unary_bits,cost_unary_bits,mant_decisions,H_mant_bits,cost_mant_bits\n" });
        for ctx in 0..nctx {
            let mut dn = [0u64; 3];
            let mut dh = [0f64; 3];
            let mut dc = [0f64; 3];
            for node in 0..64 {
                let e = cnt[pi][node * 4096 + ctx];
                let n = e[0] + e[1];
                if n == 0 {
                    continue;
                }
                let cl = if node == 0 { 0 } else if node <= 16 { 1 } else { 2 };
                dn[cl] += n;
                let p = e[1] as f64 / n as f64;
                dh[cl] += if p > 0.0 && p < 1.0 { -(n as f64) * (p * p.log2() + (1.0 - p) * (1.0 - p).log2()) } else { 0.0 };
                dc[cl] += cost[node * 4096 + ctx];
            }
            if dn[0] == 0 {
                continue;
            }
            let f = feat[ctx];
            let e0 = cnt[pi][ctx];
            csv += &format!(
                "{},{},{},{:.2},{:.2},{:.4},{:.0},{:.0},{},{:.0},{:.0},{},{:.0},{:.0}\n",
                ctx / stride, ctx % stride, dn[0], f[1] / f[0], f[2] / f[0], e0[0] as f64 / dn[0] as f64, dh[0], dc[0], dn[1], dh[1], dc[1], dn[2], dh[2], dc[2]
            );
        }
        std::fs::write(out.join(format!("contexts_{nm}.csv")), csv).unwrap();
    }
    // marginal occupancy of each quantiser level + refinement tests
    let marg = |key: &dyn Fn(usize) -> usize, nl: usize, nctx: usize, pi: usize| -> Vec<u64> {
        let mut v = vec![0u64; nl];
        for ctx in 0..nctx {
            let e = cnt[pi][ctx];
            v[key(ctx)] += e[0] + e[1];
        }
        v
    };
    let pct = |v: &[u64]| -> String {
        let t: u64 = v.iter().sum();
        v.iter().enumerate().filter(|(_, &c)| c > 0).map(|(i, &c)| format!("{i}:{:.1}%", c as f64 / t as f64 * 100.0)).collect::<Vec<_>>().join(" ")
    };
    println!("  qs occupancy (pixels): {}", pct(&marg(&|c| c / ND, NQ, NQ * ND, 0)));
    println!("  qd occupancy:          {}", pct(&marg(&|c| c % ND, ND, NQ * ND, 0)));
    println!("  qe occupancy:          {}", pct(&marg(&|c| c / NW, NQ, NQ * NW, 1)));
    println!("  qw occupancy:          {}", pct(&marg(&|c| c % NW, NW, NQ * NW, 1)));
    println!("\n  KT code length [B] of refined partitions vs current (positive gain = finer key would help, before any mixer interaction):");
    for (cur, fine, what) in [(0usize, 2usize, "B: qs 4 levels/octave"), (0, 3, "B: qd extended to 12 levels"), (1, 4, "C: qe 4 levels/octave"), (1, 5, "C: qw extended to 12 levels")] {
        let g: Vec<f64> = (0..3).map(|c| (kt_part[cur][c] - kt_part[fine][c]) / 8.0).collect();
        println!("  {what:<30} gain zero {:+8.0}  unary {:+8.0}  mant {:+8.0}  (total {:+.0} B = {:.3}% of model cost)", g[0], g[1], g[2], g.iter().sum::<f64>(), g.iter().sum::<f64>() * 8.0 / total_cost * 100.0);
    }
    for (cur, fine, what) in [(0usize, 6usize, "B + 4-way W/N zero state"), (1, 7, "C + 4-way W/N zero state"), (0, 8, "B + N-zero flag"), (1, 9, "C + N-zero flag")] {
        let g = (kt_part[cur][0] - kt_part[fine][0]) / 8.0;
        println!("  {what:<30} gain on zero flag {:+8.0} B ({:.3}% of zero-flag model cost)", g, g * 8.0 / class_cost[0] * 100.0);
    }
    let _ = hbin;
}

// ------------------------------------------------------------------ variants
fn parse_var(s: &str) -> Cfg {
    match s {
        "base" => BASE,
        "znz" => Cfg { znz: true, ..BASE },
        "c16" => Cfg { c16: true, ..BASE },
        "bqd10" => Cfg { bqd10: true, ..BASE },
        "znz+bqd10" => Cfg { znz: true, bqd10: true, ..BASE },
        "c16+bqd10" => Cfg { c16: true, bqd10: true, ..BASE },
        _ => panic!("unknown variant {s}"),
    }
}

fn variant(imgs: &[Img], cfg: Cfg, reps: usize, out: &PathBuf, label: &str) {
    let mut rows = String::from("image,baseline_bytes,variant_bytes,delta_bytes,delta_pct,lossless,deterministic\n");
    let (mut tb, mut tv) = (0usize, 0usize);
    let (mut te_b, mut te_v, mut td_b, mut td_v) = (vec![], vec![], vec![], vec![]);
    let mut all_ok = true;
    for _ in 0..reps {
        let (mut eb, mut ev, mut db, mut dv) = (0f64, 0f64, 0f64, 0f64);
        for im in imgs {
            let t = Instant::now();
            let sb = encode_img(im, BASE);
            eb += t.elapsed().as_secs_f64();
            let t = Instant::now();
            let sv = encode_img(im, cfg);
            ev += t.elapsed().as_secs_f64();
            // decode timing (Stage 4 + Stage 3 only, residual domain)
            let tm = |c: Cfg, s: &[Vec<u8>]| {
                let t = Instant::now();
                for (k, st) in s.iter().enumerate() {
                    let mut m = M::new(im.w, im.h, im.bits[k] as u32, c, false);
                    let mut d = Stage4Dec::new(st);
                    let mut acc = 0i32;
                    for _ in 0..im.w * im.h {
                        acc ^= m.code_sample::<_, false>(&mut d, 0);
                    }
                    std::hint::black_box(acc);
                }
                t.elapsed().as_secs_f64()
            };
            db += tm(BASE, &sb);
            dv += tm(cfg, &sv);
        }
        te_b.push(eb);
        te_v.push(ev);
        td_b.push(db);
        td_v.push(dv);
    }
    for im in imgs {
        let sb = encode_img(im, BASE);
        let sv = encode_img(im, cfg);
        let sv2 = encode_img(im, cfg);
        let (b, v) = (sb.iter().map(|s| s.len()).sum::<usize>(), sv.iter().map(|s| s.len()).sum::<usize>());
        let lossless = decode_img(im, cfg, &sv);
        let det = sv == sv2;
        let ident = sb.concat() == im.prod_payload;
        all_ok &= lossless && det && ident;
        tb += b;
        tv += v;
        rows += &format!("{},{b},{v},{},{:.4},{lossless},{det}\n", im.name, v as i64 - b as i64, (v as f64 / b as f64 - 1.0) * 100.0);
        println!("{:<10} base {b:>8} B  variant {v:>8} B  {:+6} B ({:+.4}%)  lossless={lossless} deterministic={det} base_identical_to_production={ident}", im.name, v as i64 - b as i64, (v as f64 / b as f64 - 1.0) * 100.0);
    }
    let med = |v: &mut Vec<f64>| {
        v.sort_by(|a, b| a.partial_cmp(b).unwrap());
        v[v.len() / 2]
    };
    println!("{label}: TOTAL base {tb} B  variant {tv} B  {:+} B ({:+.4}%)  all_ok={all_ok}", tv as i64 - tb as i64, (tv as f64 / tb as f64 - 1.0) * 100.0);
    println!("  Stage3+4 encode median of {reps}: base {:.3}s  variant {:.3}s | decode: base {:.3}s  variant {:.3}s", med(&mut te_b), med(&mut te_v), med(&mut td_b), med(&mut td_v));
    std::fs::write(out.join(format!("variant_{label}.csv")), rows).unwrap();
    let _ = tm_unused();
}
fn tm_unused() {}

fn main() {
    if cfg!(debug_assertions) {
        eprintln!("use --release");
        std::process::exit(2);
    }
    let a: Vec<String> = std::env::args().collect();
    let arg = |k: &str| a.iter().position(|x| x == k).map(|i| a[i + 1].clone());
    let out = PathBuf::from(arg("--out").unwrap());
    std::fs::create_dir_all(&out).unwrap();
    let refdir = PathBuf::from(arg("--ref").unwrap_or("benchmarks/ellie-bc-integration/ab3/bc255s11".into()));
    let imgs = load3(&refdir);
    let _ = imgs.iter().map(|i| i.prod_file).sum::<usize>();
    match arg("--mode").unwrap().as_str() {
        "audit" => audit(&imgs, &out),
        "variant" => {
            let v = arg("--var").unwrap();
            variant(&imgs, parse_var(&v), arg("--reps").map(|s| s.parse().unwrap()).unwrap_or(3), &out, &v);
        }
        m => panic!("unknown mode {m}"),
    }
}
