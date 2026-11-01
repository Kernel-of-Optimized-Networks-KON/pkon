//! ELLIE Pareto experiment (Game 3). Experiment-only: a scratch copy of the production Stage 3 whose adaptation / complexity
//! parameters are exposed, driven by the unchanged production Stage 1, Stage 2, Stage 4 and YCoCg-R. Production src/ellie.rs is
//! not touched; the scratch Stage 3 at its default configuration is verified byte-identical to production on every image.
//!
//!   exp_ellie_pareto --set 3|50 --reps N --cfgs <file> --out <csv>
//! cfg file lines:  name | Y | Co | Cg   with plane spec  m<mask>b<mb>l<limit>s<shift>   (or `name | PROD` for production EllieCodec)
//!   mask: bit0 = ctx A (qS), bit1 = B (qS x qd), bit2 = C (qE x qW), bit3 = D (order-0 root); 15 = production
//!   mb:   modelled mantissa bits (2 = production, 1, 0)       l: slot count limit (127 = production)     s: mixer shift (10 = production)
use nanoscope::canon::{Canonical, Spec};
use nanoscope::ellie::{self, EllieCodec, Stage1, Stage2, Stage4Dec, Stage4Enc};
use nanoscope::rangecoder::BitIo;
use std::path::PathBuf;
use std::time::Instant;

// ---- constants copied verbatim from src/ellie.rs Stage 3 ----
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
#[inline(always)]
fn s3_update_slot(s: &mut u32, bit: u32, limit: u32) {
    let n = *s & 1023;
    let p = (*s >> 10) as i64;
    let target = if bit != 0 { (1i64 << 22) - 1 } else { 0 };
    let p = p + (((target - p) * RECIP[(n & 255) as usize] as i64) >> 16);
    *s = ((p as u32) << 10) | (n + (n < limit) as u32);
}
#[inline(always)]
fn s3_input(s: u32) -> i32 {
    let n = ((s & 1023) as usize).min(4);
    (STRETCH[(s >> 20) as usize] as i32 * CONF[n]) >> 3
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Cfg {
    mask: u8,
    mb: u8,
    limit: u32,
    shift: u32,
}
impl Cfg {
    fn parse(s: &str) -> Cfg {
        let s = s.trim().strip_prefix('m').expect("plane spec m<mask>b<mb>l<limit>s<shift>");
        let (m, r) = s.split_once('b').unwrap();
        let (b, r) = r.split_once('l').unwrap();
        let (l, sh) = r.split_once('s').unwrap();
        let c = Cfg { mask: m.parse().unwrap(), mb: b.parse().unwrap(), limit: l.parse().unwrap(), shift: sh.parse().unwrap() };
        assert!(c.limit <= 255 && c.mb <= 2 && c.mask != 0);
        c
    }
}

/// Scratch Stage 3: identical structure and arithmetic to production, with MASK (which of the four context slots exist)
/// and MB (modelled mantissa bits) as compile-time structure and limit / shift as runtime constants.
struct S3x<const MASK: u8, const MB: u8> {
    w: usize,
    kmax: u32,
    rmax: u32,
    x: usize,
    e: i32,
    limit: u32,
    shift: u32,
    prev_m: Vec<u32>,
    cur_m: Vec<u32>,
    prev_s: Vec<u8>,
    cur_s: Vec<u8>,
    t: Vec<u32>,
    sg: [u32; 9],
    wt: Vec<[i32; S3_IN]>,
}
impl<const MASK: u8, const MB: u8> S3x<MASK, MB> {
    fn new(width: usize, bits: u32, cfg: Cfg) -> Self {
        let rmax = 2 * ((1u32 << bits) - 1);
        Self {
            w: width,
            kmax: 32 - rmax.leading_zeros(),
            rmax,
            x: 0,
            e: 0,
            limit: cfg.limit,
            shift: cfg.shift,
            prev_m: vec![0; width + 2],
            cur_m: vec![0; width + 2],
            prev_s: vec![1; width + 2],
            cur_s: vec![1; width + 2],
            t: vec![SLOT_INIT; S3_SLOTS],
            sg: [SLOT_INIT; 9],
            wt: vec![[20000, 20000, 20000, 20000, 0]; 4 * 64],
        }
    }
    #[inline(always)]
    fn code_bit<I: BitIo>(&mut self, io: &mut I, idx: [usize; 3], wb: usize, node: usize, want: u32) -> u32 {
        let (ia, ib, ic) = (idx[0] + node, idx[1] + node, idx[2] + node);
        let id = OFF_D + node;
        let (limit, shift) = (self.limit, self.shift);
        let (t, wt) = (&mut self.t[..], &mut self.wt[..]);
        // SAFETY: same index bounds as production (node <= 62, caps on qS/qd/qE/qW, wb + node < 256).
        let (sa, sb, sc, sd) = unsafe {
            (
                if MASK & 1 != 0 { *t.get_unchecked(ia) } else { SLOT_INIT },
                if MASK & 2 != 0 { *t.get_unchecked(ib) } else { SLOT_INIT },
                if MASK & 4 != 0 { *t.get_unchecked(ic) } else { SLOT_INIT },
                if MASK & 8 != 0 { *t.get_unchecked(id) } else { SLOT_INIT },
            )
        };
        let inp = [
            if MASK & 1 != 0 { s3_input(sa) } else { 0 },
            if MASK & 2 != 0 { s3_input(sb) } else { 0 },
            if MASK & 4 != 0 { s3_input(sc) } else { 0 },
            if MASK & 8 != 0 { s3_input(sd) } else { 0 },
            256,
        ];
        let ws = unsafe { wt.get_unchecked_mut(wb + node) };
        let dot = (inp[0] * ws[0]) as i64 + (inp[1] * ws[1]) as i64 + (inp[2] * ws[2]) as i64 + (inp[3] * ws[3]) as i64 + (inp[4] * ws[4]) as i64;
        let x = (dot >> 16).clamp(-2047, 2047) as i32;
        let p12 = SQUASH[(x + 2048) as usize] as i32;
        let p16 = (p12 * 16 + 8).clamp(32, 65535 - 32);
        let bit = io.code(p16 as u32, want);
        let err = ((bit as i32) << 12) - p12;
        for i in 0..S3_IN {
            ws[i] = (ws[i] + ((inp[i] * err) >> shift)).clamp(-S3_W_CLAMP, S3_W_CLAMP);
        }
        unsafe {
            if MASK & 1 != 0 {
                s3_update_slot(t.get_unchecked_mut(ia), bit, limit);
            }
            if MASK & 2 != 0 {
                s3_update_slot(t.get_unchecked_mut(ib), bit, limit);
            }
            if MASK & 4 != 0 {
                s3_update_slot(t.get_unchecked_mut(ic), bit, limit);
            }
            if MASK & 8 != 0 {
                s3_update_slot(t.get_unchecked_mut(id), bit, limit);
            }
        }
        bit
    }

    #[inline(always)]
    fn code_sample<I: BitIo>(&mut self, io: &mut I, r: i32) -> i32 {
        let x = self.x;
        let i = x + 1;
        if x == 0 {
            self.cur_m[0] = self.prev_m[1];
            self.cur_s[0] = 1;
        }
        let (aw, an, anw, ane) = (self.cur_m[i - 1], self.prev_m[i], self.prev_m[i - 1], self.prev_m[i + 1]);
        let sum = 2 * aw + 2 * an + anw + ane;
        let qs = s3q(sum).min(NQ - 1);
        let qd = s3q(aw.abs_diff(an)).min(ND - 1);
        let qe = s3q((self.e >> 2) as u32).min(NQ - 1);
        let qw = s3q(aw).min(NW - 1);
        let idx = [OFF_A + qs * 64, OFF_B + (qs * ND + qd) * 64, OFF_C + (qe * NW + qw) * 64];
        let wb = (qs >> 3) * 64;
        let sgi = self.cur_s[i - 1] as usize * 3 + self.prev_s[i] as usize;

        let a_in = r.unsigned_abs();
        let k_in = 32 - a_in.leading_zeros();
        let nz = self.code_bit(io, idx, wb, 0, (a_in != 0) as u32);
        let mut a = 0u32;
        let mut neg = 0u32;
        if nz != 0 {
            let mut k = 1u32;
            while k < self.kmax {
                if self.code_bit(io, idx, wb, k as usize, (k_in > k) as u32) == 0 {
                    break;
                }
                k += 1;
            }
            a = 1;
            if k >= 2 {
                let raw;
                if MB >= 1 {
                    let t = self.code_bit(io, idx, wb, 15 + k as usize, (a_in >> (k - 2)) & 1);
                    a = 2 + t;
                    if k >= 3 {
                        if MB >= 2 {
                            let u = self.code_bit(io, idx, wb, 33 + 2 * (k as usize - 3) + t as usize, (a_in >> (k - 3)) & 1);
                            a = 2 * a + u;
                            raw = k - 3;
                        } else {
                            raw = k - 2;
                        }
                    } else {
                        raw = 0;
                    }
                } else {
                    raw = k - 1;
                }
                for b in (0..raw).rev() {
                    a = 2 * a + io.code(32768, (a_in >> b) & 1);
                }
            }
            let ps = (self.sg[sgi] >> 16).clamp(32, 65535 - 32);
            neg = io.code(ps, (r < 0) as u32);
            s3_update_slot(&mut self.sg[sgi], neg, self.limit);
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
    fn encode_plane<I: BitIo>(&mut self, res: &[i32], io: &mut I) {
        let lim = self.rmax;
        assert!(res.iter().all(|&r| r.unsigned_abs() <= lim));
        for &r in res {
            self.code_sample(io, r);
        }
    }
    fn decode_plane<I: BitIo>(&mut self, out: &mut [i32], io: &mut I) {
        for o in out.iter_mut() {
            *o = self.code_sample(io, 0);
        }
    }
}

macro_rules! dispatch {
    ($cfg:expr, $f:ident, $($args:expr),*) => {
        match ($cfg.mask, $cfg.mb) {
            (15, 2) => $f::<15, 2>($($args),*), (15, 1) => $f::<15, 1>($($args),*), (15, 0) => $f::<15, 0>($($args),*),
            (13, 2) => $f::<13, 2>($($args),*), (13, 1) => $f::<13, 1>($($args),*), (13, 0) => $f::<13, 0>($($args),*),
            (11, 2) => $f::<11, 2>($($args),*), (11, 1) => $f::<11, 1>($($args),*), (11, 0) => $f::<11, 0>($($args),*),
            (7, 2) => $f::<7, 2>($($args),*), (7, 1) => $f::<7, 1>($($args),*), (7, 0) => $f::<7, 0>($($args),*),
            (9, 2) => $f::<9, 2>($($args),*), (9, 1) => $f::<9, 1>($($args),*), (9, 0) => $f::<9, 0>($($args),*),
            (12, 2) => $f::<12, 2>($($args),*), (12, 1) => $f::<12, 1>($($args),*), (12, 0) => $f::<12, 0>($($args),*),
            (10, 2) => $f::<10, 2>($($args),*), (10, 1) => $f::<10, 1>($($args),*), (10, 0) => $f::<10, 0>($($args),*),
            (5, 2) => $f::<5, 2>($($args),*), (5, 1) => $f::<5, 1>($($args),*), (5, 0) => $f::<5, 0>($($args),*),
            (8, 2) => $f::<8, 2>($($args),*), (8, 1) => $f::<8, 1>($($args),*), (8, 0) => $f::<8, 0>($($args),*),
            (14, 2) => $f::<14, 2>($($args),*), (14, 1) => $f::<14, 1>($($args),*), (14, 0) => $f::<14, 0>($($args),*),
            (6, 2) => $f::<6, 2>($($args),*), (6, 1) => $f::<6, 1>($($args),*), (6, 0) => $f::<6, 0>($($args),*),
            (3, 2) => $f::<3, 2>($($args),*), (3, 1) => $f::<3, 1>($($args),*), (3, 0) => $f::<3, 0>($($args),*),
            (2, 2) => $f::<2, 2>($($args),*), (2, 1) => $f::<2, 1>($($args),*), (2, 0) => $f::<2, 0>($($args),*),
            (4, 2) => $f::<4, 2>($($args),*), (4, 1) => $f::<4, 1>($($args),*), (4, 0) => $f::<4, 0>($($args),*),
            (m, b) => panic!("mask {m} mb {b} not instantiated"),
        }
    };
}
fn enc_plane_g<const M: u8, const B: u8>(cfg: Cfg, w: usize, bits: u32, res: &[i32], enc: &mut Stage4Enc) {
    S3x::<M, B>::new(w, bits, cfg).encode_plane(res, enc);
}
fn dec_plane_g<const M: u8, const B: u8>(cfg: Cfg, w: usize, bits: u32, res: &mut [i32], dec: &mut Stage4Dec) {
    S3x::<M, B>::new(w, bits, cfg).decode_plane(res, dec);
}

// ---- end-to-end codec (transform + per-plane Stage 1..4 + inverse) ----
#[derive(Clone)]
enum Engine {
    Prod,
    Scratch([Cfg; 3]),
}
struct Encoded {
    payload: Vec<u8>,
    lens: Vec<usize>,
}

fn plane_bits(k: usize) -> u32 {
    if k == 1 || k == 2 { ellie::YCOCG_CHROMA_BITS } else { 8 }
}

fn encode(img: &Canonical, eng: &Engine) -> Encoded {
    let Spec { width, height, .. } = img.spec;
    let (w, h) = (width as usize, height as usize);
    let n = w * h;
    let ch = img.spec.layout.channels();
    let mut planes = vec![vec![0u16; n]; ch];
    for (i, px) in img.data.chunks_exact(ch).enumerate() {
        for k in 0..ch {
            planes[k][i] = px[k] as u16;
        }
    }
    assert!(ellie::ycocg_r_forward_planes(&mut planes[..3]));
    let mut payload = vec![];
    let mut lens = vec![0; ch];
    let mut enc = Stage4Enc::with_capacity(n);
    match eng {
        Engine::Prod => {
            let mut codecs: Vec<(u32, EllieCodec)> = vec![];
            for (k, plane) in planes.iter().enumerate() {
                let bits = plane_bits(k);
                let at = match codecs.iter().position(|c| c.0 == bits) {
                    Some(i) => i,
                    None => {
                        codecs.push((bits, EllieCodec::new(w, h, bits).unwrap()));
                        codecs.len() - 1
                    }
                };
                enc.reset();
                codecs[at].1.encode_planes(&[plane.as_slice()], &mut enc).unwrap();
                let b = enc.finish();
                lens[k] = b.len();
                payload.extend_from_slice(b);
            }
        }
        Engine::Scratch(cfgs) => {
            let mut s1: Vec<(u32, Stage1)> = vec![];
            let mut res = vec![0i32; n];
            for (k, plane) in planes.iter().enumerate() {
                let bits = plane_bits(k);
                let at = match s1.iter().position(|c| c.0 == bits) {
                    Some(i) => i,
                    None => {
                        s1.push((bits, Stage1::new(w, h, bits).unwrap()));
                        s1.len() - 1
                    }
                };
                s1[at].1.encode(plane, &mut res).unwrap();
                Stage2::new().encode_in_place(&mut res);
                enc.reset();
                let c = cfgs[k.min(2) * (k < 3) as usize];
                dispatch!(c, enc_plane_g, c, w, bits, &res, &mut enc);
                let b = enc.finish();
                lens[k] = b.len();
                payload.extend_from_slice(b);
            }
        }
    }
    Encoded { payload, lens }
}

fn decode(e: &Encoded, w: usize, h: usize, eng: &Engine) -> Vec<u8> {
    let n = w * h;
    let ch = e.lens.len();
    let mut planes = vec![vec![0u16; n]; ch];
    let mut off = 0;
    match eng {
        Engine::Prod => {
            let mut codecs: Vec<(u32, EllieCodec)> = vec![];
            for k in 0..ch {
                let bits = plane_bits(k);
                let at = match codecs.iter().position(|c| c.0 == bits) {
                    Some(i) => i,
                    None => {
                        codecs.push((bits, EllieCodec::new(w, h, bits).unwrap()));
                        codecs.len() - 1
                    }
                };
                codecs[at].1.decode_planes(&e.payload[off..off + e.lens[k]], &mut [planes[k].as_mut_slice()]).unwrap();
                off += e.lens[k];
            }
        }
        Engine::Scratch(cfgs) => {
            let mut s1: Vec<(u32, Stage1)> = vec![];
            let mut res = vec![0i32; n];
            for k in 0..ch {
                let bits = plane_bits(k);
                let at = match s1.iter().position(|c| c.0 == bits) {
                    Some(i) => i,
                    None => {
                        s1.push((bits, Stage1::new(w, h, bits).unwrap()));
                        s1.len() - 1
                    }
                };
                let mut dec = Stage4Dec::new(&e.payload[off..off + e.lens[k]]);
                let c = cfgs[k.min(2) * (k < 3) as usize];
                dispatch!(c, dec_plane_g, c, w, bits, &mut res, &mut dec);
                Stage2::new().decode_in_place(&mut res);
                s1[at].1.decode(&res, &mut planes[k]).unwrap();
                off += e.lens[k];
            }
        }
    }
    assert!(ellie::ycocg_r_inverse_planes(&mut planes[..3]));
    let mut out = vec![0u8; n * ch];
    for i in 0..n {
        for k in 0..ch {
            out[i * ch + k] = planes[k][i] as u8;
        }
    }
    out
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

fn load(set: &str) -> Vec<(String, Canonical)> {
    let data = PathBuf::from("data");
    if set == "3" {
        return ["portrait_0.jpg", "screen_0.png", "night_0.jpg"]
            .iter()
            .map(|f| (f.split('.').next().unwrap().to_string(), center_crop(&Canonical::load(&data.join(f)).unwrap(), 1024)))
            .collect();
    }
    let m: Vec<serde_json::Value> = serde_json::from_str(&std::fs::read_to_string(data.join("dataset50.json")).unwrap()).unwrap();
    m.iter().map(|m| (m["name"].as_str().unwrap().to_string(), center_crop(&Canonical::load(&data.join(m["file"].as_str().unwrap())).unwrap(), 1024))).collect()
}

fn main() {
    if cfg!(debug_assertions) {
        eprintln!("use --release");
        std::process::exit(2);
    }
    let a: Vec<String> = std::env::args().collect();
    let arg = |k: &str| a.iter().position(|x| x == k).map(|i| a[i + 1].clone());
    let set = arg("--set").unwrap_or("3".into());
    let reps: usize = arg("--reps").unwrap_or("5".into()).parse().unwrap();
    let out = arg("--out").unwrap();
    let mut cfgs: Vec<(String, Engine)> = vec![];
    for l in std::fs::read_to_string(arg("--cfgs").unwrap()).unwrap().lines() {
        let l = l.trim();
        if l.is_empty() || l.starts_with('#') {
            continue;
        }
        let p: Vec<&str> = l.split('|').map(str::trim).collect();
        if p.len() == 2 && p[1] == "PROD" {
            cfgs.push((p[0].into(), Engine::Prod));
        } else {
            cfgs.push((p[0].into(), Engine::Scratch([Cfg::parse(p[1]), Cfg::parse(p[2]), Cfg::parse(p[3])])));
        }
    }
    let imgs = load(&set);
    let mut csv = String::from("cfg,image,bytes,y_bytes,co_bytes,cg_bytes,enc_min_s,enc_med_s,dec_min_s,dec_med_s,exact,det,prod_identical\n");
    for (name, img) in &imgs {
        let (w, h) = (img.spec.width as usize, img.spec.height as usize);
        let nc = cfgs.len();
        let mut et = vec![vec![]; nc];
        let mut dt = vec![vec![]; nc];
        let mut first: Vec<Option<Encoded>> = (0..nc).map(|_| None).collect();
        let (mut exact, mut det) = (vec![true; nc], vec![true; nc]);
        for rep in 0..reps {
            for j in 0..nc {
                let c = (j + rep) % nc; // rotate order each rep so configs are interleaved and none always runs first
                let t = Instant::now();
                let e = encode(img, &cfgs[c].1);
                et[c].push(t.elapsed().as_secs_f64());
                let t = Instant::now();
                let back = decode(&e, w, h, &cfgs[c].1);
                dt[c].push(t.elapsed().as_secs_f64());
                exact[c] &= back == img.data;
                match &first[c] {
                    None => first[c] = Some(e),
                    Some(f) => det[c] &= f.payload == e.payload && f.lens == e.lens,
                }
            }
        }
        let prod_enc = encode(img, &Engine::Prod);
        for c in 0..nc {
            let f = first[c].as_ref().unwrap();
            let med = |v: &mut Vec<f64>| {
                v.sort_by(|a, b| a.partial_cmp(b).unwrap());
                v[v.len() / 2]
            };
            let (emin, dmin) = (et[c].iter().cloned().fold(f64::MAX, f64::min), dt[c].iter().cloned().fold(f64::MAX, f64::min));
            let (emed, dmed) = (med(&mut et[c]), med(&mut dt[c]));
            let ident = f.payload == prod_enc.payload;
            csv += &format!(
                "{},{},{},{},{},{},{:.5},{:.5},{:.5},{:.5},{},{},{}\n",
                cfgs[c].0, name, f.payload.len(), f.lens[0], f.lens[1], f.lens[2], emin, emed, dmin, dmed, exact[c], det[c], ident
            );
        }
        eprintln!("{name} done");
    }
    std::fs::write(&out, csv).unwrap();
}
