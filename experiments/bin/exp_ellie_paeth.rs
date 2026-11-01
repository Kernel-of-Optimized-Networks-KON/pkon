//! ELLIE Stage 1 Paeth-predictor experiment (isolated; production Stage 1 and container untouched).
//!
//! Question: does blending a Paeth predictor into ELLIE's diagonal estimate reduce actual encoded size, and is the gain
//! worth its compute cost?
//!
//! `S1<false>` is a verbatim copy of `ellie::Stage1` (checked bit-exact against production in the self-test).
//! `S1<true>` changes exactly one line of `predict`: the diagonal estimate D becomes the rounded average of the existing
//! D estimate and the Paeth pick (A=W, B=N, C=NW). Horizontal/vertical estimates, activity measures, weights, clamp and
//! fold are untouched. No new state, passes or allocations. Stages 2-4 are the production ones.
//!
//! Sizes: payload = sum of per-plane Stage 4 streams. Container overhead (32-byte header + ELLIE metadata) depends only on
//! channel count and representation, so it is taken from the production container; the baseline file size computed here is
//! asserted equal to the production `.pkon` length, which validates the accounting used for the candidate.
//!
//! usage: exp_ellie_paeth [--out DIR] [--reps N] [FILE...]   (release only)
use nanoscope::canon::{first_mismatch, Canonical, Spec};
use nanoscope::ellie::{self, Stage1, Stage2, Stage3, Stage4Dec, Stage4Enc};
use nanoscope::pkon::{self, EllieRepr};
use std::fmt::Write as _;
use std::path::PathBuf;
use std::time::Instant;

const CROP: u32 = 1024;
const MAX_BITS: u32 = 16;

// ------------------------------------------------------------------ Stage 1 copy with optional Paeth
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

#[inline(always)]
fn predict<const P: bool>(m: i32, w: i32, ww: i32, n: i32, nn: i32, nw: i32, ne: i32, s_h: u32, s_v: u32, s45: u32, s135: u32) -> i32 {
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
    let h4 = (4 * w + ((w - ww + n - nw) >> 1) + o) as u64;
    let v4 = (4 * n + ((n - nn + w - nw) >> 1) + o) as u64;
    let mut d4 = (3 * ((4 - f) * nw + f * ne)) / 4 + (w + ne - n) + o;
    if P {
        // Both operands are >= 0 (d4 carries +o, paeth*4 is in [0, 4m] and gets +o); the average stays in range.
        d4 = (d4 + 4 * paeth(w, n, nw) + o + 1) >> 1;
    }
    let d4 = d4 as u64;

    let t = (ph * h4 + pv * v4 + pd * d4) / d;
    (((t + 2) >> 2) as i64 - m as i64).clamp(0, m as i64) as i32
}

struct S1<const P: bool> {
    w: usize,
    h: usize,
    bits: u32,
    m: u32,
    ra: Vec<u16>,
    rb: Vec<u16>,
    rc: Vec<u16>,
    da: Vec<[u16; 4]>,
    db: Vec<[u16; 4]>,
}

impl<const P: bool> S1<P> {
    fn new(width: usize, height: usize, bits: u32) -> Self {
        assert!((1..=MAX_BITS).contains(&bits));
        Self { w: width, h: height, bits, m: (1u32 << bits) - 1, ra: vec![0; width + 4], rb: vec![0; width + 4], rc: vec![0; width + 4], da: vec![[0; 4]; width + 1], db: vec![[0; 4]; width + 1] }
    }

    fn encode(&mut self, src: &[u16], res: &mut [i32]) {
        assert!(src.len() == self.w * self.h && res.len() == src.len());
        self.scan(|i, p| {
            let x = src[i];
            res[i] = x as i32 - p as i32;
            x as u32
        });
    }

    /// Returns true if any residual reconstructed out of range.
    fn decode(&mut self, res: &[i32], out: &mut [u16]) -> bool {
        let m = self.m as i32;
        let mut bad = false;
        self.scan(|i, p| {
            let v = p as i32 + res[i];
            bad |= (v as u32) > m as u32;
            let v = v.clamp(0, m);
            out[i] = v as u16;
            v as u32
        });
        bad
    }

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

        let x0 = resolve(0, mid) as u16;
        cur[2] = x0;
        cur[0] = x0;
        cur[1] = x0;
        for x in 1..w {
            let wv = cur[x + 1] as i32;
            let wwv = cur[x] as i32;
            let pr = ((5 * wv - wwv + 2) >> 2).clamp(0, m);
            cur[x + 2] = resolve(x, pr as u32) as u16;
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
                let pr = predict::<P>(m, n, n, n, nn, n, ne, s_h, s_v, s45, s135);
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
                let pr = predict::<P>(m, wv, wwv, n, nn, nw, ne, s_h, s_v, s45, s135);
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

            let last = cur[w + 1];
            cur[w + 2] = last;
            cur[w + 3] = last;
            dcur[w] = [0, v_c as u16, 0, v_c as u16];

            std::mem::swap(&mut pp, &mut p);
            std::mem::swap(&mut p, &mut cur);
            std::mem::swap(&mut dprev, &mut dcur);
        }
    }
}

// ------------------------------------------------------------------ pipeline (Stage 1 variant + production Stages 2-4)
/// One plane -> one Stage 4 stream, mirroring `EllieCodec::encode_planes` for a single plane.
fn encode_plane<const P: bool>(s1: &mut S1<P>, s3: &mut Stage3, res: &mut [i32], enc: &mut Stage4Enc, plane: &[u16]) -> Vec<u8> {
    enc.reset();
    s3.reset();
    s1.encode(plane, res);
    Stage2::new().encode_in_place(res);
    s3.encode_plane(res, enc).unwrap();
    enc.finish().to_vec()
}

fn decode_plane<const P: bool>(s1: &mut S1<P>, s3: &mut Stage3, res: &mut [i32], stream: &[u8], out: &mut [u16]) {
    let mut dec = Stage4Dec::new(stream);
    s3.reset();
    s3.decode_plane(res, &mut dec).unwrap();
    Stage2::new().decode_in_place(res);
    assert!(!s1.decode(res, out), "residual out of range");
}

fn plane_bits(ycocg: bool, k: usize) -> u32 {
    if ycocg && (k == 1 || k == 2) { 9 } else { 8 }
}

/// Per-plane streams for one representation.
fn encode_repr<const P: bool>(planes: &[Vec<u16>], w: usize, h: usize, ycocg: bool) -> Vec<Vec<u8>> {
    let mut work = planes.to_vec();
    if ycocg {
        assert!(ellie::ycocg_r_forward_planes(&mut work[..3]));
    }
    let mut out = vec![];
    let mut res = vec![0i32; w * h];
    let mut enc = Stage4Enc::with_capacity(w * h);
    let mut cache: Vec<(u32, S1<P>, Stage3)> = vec![];
    for (k, plane) in work.iter().enumerate() {
        let bits = plane_bits(ycocg, k);
        let at = match cache.iter().position(|c| c.0 == bits) {
            Some(i) => i,
            None => {
                cache.push((bits, S1::<P>::new(w, h, bits), Stage3::new(w, h, bits).unwrap()));
                cache.len() - 1
            }
        };
        let (_, s1, s3) = &mut cache[at];
        out.push(encode_plane(s1, s3, &mut res, &mut enc, plane));
    }
    out
}

fn decode_repr<const P: bool>(streams: &[Vec<u8>], w: usize, h: usize, ycocg: bool) -> Vec<Vec<u16>> {
    let nch = streams.len();
    let mut planes = vec![vec![0u16; w * h]; nch];
    let mut res = vec![0i32; w * h];
    let mut cache: Vec<(u32, S1<P>, Stage3)> = vec![];
    for k in 0..nch {
        let bits = plane_bits(ycocg, k);
        let at = match cache.iter().position(|c| c.0 == bits) {
            Some(i) => i,
            None => {
                cache.push((bits, S1::<P>::new(w, h, bits), Stage3::new(w, h, bits).unwrap()));
                cache.len() - 1
            }
        };
        let (_, s1, s3) = &mut cache[at];
        decode_plane(s1, s3, &mut res, &streams[k], &mut planes[k]);
    }
    if ycocg {
        assert!(ellie::ycocg_r_inverse_planes(&mut planes[..3]));
    }
    planes
}

// ------------------------------------------------------------------ self-test
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
    (0..w * h)
        .map(|i| {
            let (x, y) = ((i % w.max(1)) as u32, (i / w.max(1)) as u32);
            (match kind {
                0 => (rng.next() as u32) & m,
                1 => m / 2,
                2 => if (x + y) & 1 == 0 { 0 } else { m },
                3 => (x * 7 + y * 13) & m,
                4 => if rng.next() & 7 == 0 { m } else { 0 },
                _ => ((x ^ y) * 37 + (rng.next() as u32 & 3)) & m,
            }) as u16
        })
        .collect()
}

fn self_test() -> usize {
    let mut rng = Rng(0x9E3779B97F4A7C15);
    let mut cases = 0usize;
    // Paeth against an i64 reference of the standard definition over a dense small grid.
    for a in 0..40i32 {
        for b in 0..40i32 {
            for c in 0..40i32 {
                let p = (a + b - c) as i64;
                let (pa, pb, pc) = ((p - a as i64).abs(), (p - b as i64).abs(), (p - c as i64).abs());
                let r = if pa <= pb && pa <= pc { a } else if pb <= pc { b } else { c };
                assert_eq!(paeth(a, b, c), r);
            }
        }
    }
    // Extremes at 16 bits: no overflow, result is one of the three inputs.
    for &(a, b, c) in &[(0, 65535, 0), (65535, 0, 65535), (0, 0, 65535), (65535, 65535, 0), (65535, 0, 0)] {
        let r = paeth(a, b, c);
        assert!(r == a || r == b || r == c);
    }
    let dims = [(0, 0), (0, 3), (3, 0), (1, 1), (1, 7), (7, 1), (2, 2), (2, 5), (3, 3), (5, 2), (4, 4), (17, 9), (64, 3), (3, 64)];
    for bits in 1..=MAX_BITS {
        for &(w, h) in &dims {
            for kind in 0..6 {
                let src = pattern(kind, w, h, bits, &mut rng);
                let n = w * h;
                let mut base_ref = vec![0i32; n];
                let mut a = vec![0i32; n];
                let mut b = vec![0i32; n];
                let mut b2 = vec![0i32; n];
                // The baseline copy must equal production Stage 1 exactly.
                let mut prod = Stage1::new(w, h, bits).unwrap();
                prod.encode(&src, &mut base_ref).unwrap();
                let mut s0 = S1::<false>::new(w, h, bits);
                s0.encode(&src, &mut a);
                assert_eq!(a, base_ref, "baseline copy diverges from production bits={bits} {w}x{h} kind={kind}");
                let mut s1 = S1::<true>::new(w, h, bits);
                s1.encode(&src, &mut b);
                let mut s1b = S1::<true>::new(w, h, bits);
                s1b.encode(&src, &mut b2);
                assert_eq!(b, b2, "nondeterministic");
                let m = (1i32 << bits) - 1;
                assert!(b.iter().all(|&e| (-m..=m).contains(&e)));
                // Fold/unfold unaffected: every residual is representable for its prediction.
                let mut back = vec![0u16; n];
                let mut d = S1::<true>::new(w, h, bits);
                assert!(!d.decode(&b, &mut back));
                assert_eq!(back, src, "paeth round trip bits={bits} {w}x{h} kind={kind}");
                cases += 1;
            }
        }
    }
    // Full Stage 1-4 round trip for the candidate at several depths.
    for &bits in &[1u32, 2, 5, 8, 9, 12, 16] {
        for &(w, h) in &[(1usize, 1usize), (2, 3), (9, 7), (40, 33)] {
            for kind in [0, 2, 4, 5] {
                let src = pattern(kind, w, h, bits, &mut rng);
                let mut s1 = S1::<true>::new(w, h, bits);
                let mut s3 = Stage3::new(w, h, bits).unwrap();
                let mut res = vec![0i32; w * h];
                let mut enc = Stage4Enc::new();
                let stream = encode_plane(&mut s1, &mut s3, &mut res, &mut enc, &src);
                let mut s1d = S1::<true>::new(w, h, bits);
                let mut out = vec![0u16; w * h];
                decode_plane(&mut s1d, &mut s3, &mut res, &stream, &mut out);
                assert_eq!(out, src, "full pipeline bits={bits} {w}x{h} kind={kind}");
                cases += 1;
            }
        }
    }
    cases
}

// ------------------------------------------------------------------ corpus helpers
fn utc_stamp() -> String {
    let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() as i64;
    let (days, rem) = (secs.div_euclid(86400), secs.rem_euclid(86400));
    let z = days + 719468;
    let era = z.div_euclid(146097);
    let doe = z.rem_euclid(146097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + (m <= 2) as i64;
    format!("{y:04}{m:02}{d:02}-{:02}{:02}{:02}Z", rem / 3600, rem % 3600 / 60, rem % 60)
}

fn crop(c: Canonical) -> Canonical {
    let Spec { width, height, layout, bit_depth } = c.spec;
    let (nw, nh) = (width.min(CROP), height.min(CROP));
    if (nw, nh) == (width, height) {
        return c;
    }
    let (x0, y0) = ((width - nw) as usize / 2, (height - nh) as usize / 2);
    let ch = layout.channels();
    let (w, nw_, nh_) = (width as usize, nw as usize, nh as usize);
    let mut data = Vec::with_capacity(nw_ * nh_ * ch);
    for y in 0..nh_ {
        let s = ((y0 + y) * w + x0) * ch;
        data.extend_from_slice(&c.data[s..s + nw_ * ch]);
    }
    let mut out = Canonical::new(Spec { width: nw, height: nh, layout, bit_depth }, data).unwrap();
    out.notes = c.notes;
    out
}

fn deinterleave(c: &Canonical) -> Vec<Vec<u16>> {
    let ch = c.spec.layout.channels();
    let mut planes = vec![vec![0u16; c.spec.width as usize * c.spec.height as usize]; ch];
    for (i, px) in c.data.chunks_exact(ch).enumerate() {
        for (k, &v) in px.iter().enumerate() {
            planes[k][i] = v as u16;
        }
    }
    planes
}

fn interleave(planes: &[Vec<u16>]) -> Vec<u8> {
    let ch = planes.len();
    let mut data = vec![0u8; planes[0].len() * ch];
    for (k, p) in planes.iter().enumerate() {
        for (i, &v) in p.iter().enumerate() {
            data[i * ch + k] = v as u8;
        }
    }
    data
}

struct Out {
    ycocg: bool,
    streams: Vec<Vec<u8>>,
    payload: usize,
    file: usize,
    enc_s: f64,
    dec_s: f64,
    s1_enc_s: f64,
    s1_dec_s: f64,
    exact: bool,
    det: bool,
}

/// Stage 1 only, over the planes of the chosen representation (best of `reps`).
fn s1_time<const P: bool>(planes: &[Vec<u16>], w: usize, h: usize, ycocg: bool, reps: usize) -> (f64, f64) {
    let mut work = planes.to_vec();
    if ycocg {
        ellie::ycocg_r_forward_planes(&mut work[..3]);
    }
    let (mut be, mut bd) = (f64::MAX, f64::MAX);
    let mut res: Vec<Vec<i32>> = vec![vec![0; w * h]; work.len()];
    let mut out = vec![0u16; w * h];
    for _ in 0..reps {
        let t = Instant::now();
        for (k, p) in work.iter().enumerate() {
            S1::<P>::new(w, h, plane_bits(ycocg, k)).encode(p, &mut res[k]);
        }
        be = be.min(t.elapsed().as_secs_f64());
        let t = Instant::now();
        for k in 0..work.len() {
            assert!(!S1::<P>::new(w, h, plane_bits(ycocg, k)).decode(&res[k], &mut out));
        }
        bd = bd.min(t.elapsed().as_secs_f64());
        assert_eq!(out, work[work.len() - 1]);
    }
    (be, bd)
}

/// Auto selection exactly as production: try untransformed then YCoCg-R, keep YCoCg-R only if strictly smaller.
fn run<const P: bool>(img: &Canonical, planes: &[Vec<u16>], overhead: [usize; 2], reps: usize) -> Out {
    let (w, h) = (img.spec.width as usize, img.spec.height as usize);
    let (mut enc_s, mut dec_s) = (f64::MAX, f64::MAX);
    let (mut exact, mut det) = (true, true);
    let mut first: Option<(bool, Vec<Vec<u8>>)> = None;
    for _ in 0..reps {
        let t = Instant::now();
        let plain = encode_repr::<P>(planes, w, h, false);
        let yc = encode_repr::<P>(planes, w, h, true);
        enc_s = enc_s.min(t.elapsed().as_secs_f64());
        let sz = |s: &Vec<Vec<u8>>, o: usize| s.iter().map(|v| v.len()).sum::<usize>() + o;
        let pick_y = sz(&yc, overhead[1]) < sz(&plain, overhead[0]);
        let chosen = if pick_y { yc } else { plain };
        let t = Instant::now();
        let dec = decode_repr::<P>(&chosen, w, h, pick_y);
        dec_s = dec_s.min(t.elapsed().as_secs_f64());
        exact &= interleave(&dec) == img.data;
        let mut check = Canonical::new(img.spec, interleave(&dec)).unwrap();
        check.notes.clear();
        exact &= first_mismatch(img, &check).is_none();
        match &first {
            None => first = Some((pick_y, chosen)),
            Some((y0, s0)) => det &= *y0 == pick_y && *s0 == chosen,
        }
    }
    let (ycocg, streams) = first.unwrap();
    let payload = streams.iter().map(|v| v.len()).sum();
    let (s1_enc_s, s1_dec_s) = s1_time::<P>(planes, w, h, ycocg, reps);
    Out { ycocg, payload, file: payload + overhead[ycocg as usize], streams, enc_s, dec_s, s1_enc_s, s1_dec_s, exact, det }
}

fn main() {
    if cfg!(debug_assertions) {
        eprintln!("use --release");
        std::process::exit(2);
    }
    let t = Instant::now();
    let cases = self_test();
    eprintln!("self-test: {cases} cases passed ({:.2}s)", t.elapsed().as_secs_f64());

    let mut out_root = PathBuf::from("benchmarks/ellie-paeth");
    let mut reps = 5usize;
    let mut files: Vec<PathBuf> = vec![];
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--out" => out_root = PathBuf::from(args.next().unwrap()),
            "--reps" => reps = args.next().unwrap().parse().unwrap(),
            _ => files.push(PathBuf::from(a)),
        }
    }
    if files.is_empty() {
        files = ["data/portrait_0.jpg", "data/screen_0.png", "data/night_0.jpg"].map(PathBuf::from).to_vec();
    }
    let dir = out_root.join(format!("{}-paeth", utc_stamp()));
    std::fs::create_dir_all(&dir).unwrap();

    let mut md = format!("# ELLIE Stage 1 Paeth experiment\n\nRelease, best of {reps}. Self-test: {cases} cases passed (baseline copy == production Stage 1 bit-exact; candidate round trips at bits 1-16, tiny/odd dims, adversarial patterns; full Stage 1-4 round trip at 7 depths).\n\nCandidate: diagonal estimate D := (D + 4*Paeth(W,N,NW) + 1) >> 1 (offset-adjusted); H, V, activities, weights, clamp, fold unchanged. Representation chosen by the production auto rule (YCoCg-R only if strictly smaller file), independently per version. File size = payload + production container overhead (32 B header + ELLIE metadata); baseline file size is checked equal to the production `.pkon` length.\n\n");
    md.push_str("| image | repr base/paeth | base file B | paeth file B | delta B | delta % | base payload | paeth payload | S1 enc base/paeth ms | S1 dec base/paeth ms | enc base/paeth s | dec base/paeth s |\n|---|---|---|---|---|---|---|---|---|---|---|---|\n");
    let (mut tb, mut tp, mut pb, mut pp) = (0usize, 0usize, 0usize, 0usize);
    let mut t = [0f64; 8]; // s1e b/p, s1d b/p, enc b/p, dec b/p
    let mut all_ok = true;
    for path in &files {
        let img = crop(Canonical::load_bytes(std::fs::read(path).unwrap()).unwrap());
        assert_eq!(img.spec.bit_depth, 8);
        let planes = deinterleave(&img);
        assert!(planes.len() >= 3);
        let name = path.file_stem().unwrap().to_string_lossy().to_string();
        // Overhead per representation from the production container.
        let mut overhead = [0usize; 2];
        let mut prod_len = [0usize; 2];
        for (i, mode) in [EllieRepr::Untransformed, EllieRepr::YCoCgR].into_iter().enumerate() {
            let f = pkon::encode_standard_with(&img, mode).unwrap();
            let hdr = pkon::parse_header(&f).unwrap();
            overhead[i] = pkon::FIXED_HEADER_LEN + hdr.meta_len as usize;
            prod_len[i] = f.len();
        }
        let prod_auto = pkon::encode_standard(&img).unwrap().len();
        let a = run::<false>(&img, &planes, overhead, reps);
        let b = run::<true>(&img, &planes, overhead, reps);
        assert_eq!(a.file, prod_auto, "{name}: baseline accounting != production .pkon length");
        assert_eq!(a.file, prod_len[a.ycocg as usize]);
        let ok = a.exact && b.exact && a.det && b.det;
        all_ok &= ok;
        let d = b.file as i64 - a.file as i64;
        let _ = writeln!(
            md,
            "| {name} | {}/{} | {} | {} | {d:+} | {:+.3}% | {} | {} | {:.1}/{:.1} | {:.1}/{:.1} | {:.3}/{:.3} | {:.3}/{:.3} |",
            if a.ycocg { "ycocg" } else { "plain" },
            if b.ycocg { "ycocg" } else { "plain" },
            a.file,
            b.file,
            d as f64 / a.file as f64 * 100.0,
            a.payload,
            b.payload,
            a.s1_enc_s * 1e3,
            b.s1_enc_s * 1e3,
            a.s1_dec_s * 1e3,
            b.s1_dec_s * 1e3,
            a.enc_s,
            b.enc_s,
            a.dec_s,
            b.dec_s
        );
        let _ = writeln!(md, "<!-- {name}: exact={}/{} deterministic={}/{} paeth plane bytes {:?} base {:?} -->", a.exact, b.exact, a.det, b.det, b.streams.iter().map(|s| s.len()).collect::<Vec<_>>(), a.streams.iter().map(|s| s.len()).collect::<Vec<_>>());
        tb += a.file;
        tp += b.file;
        pb += a.payload;
        pp += b.payload;
        for (k, v) in [a.s1_enc_s, b.s1_enc_s, a.s1_dec_s, b.s1_dec_s, a.enc_s, b.enc_s, a.dec_s, b.dec_s].into_iter().enumerate() {
            t[k] += v;
        }
        eprintln!("{name}: base {} paeth {} ({d:+}) exact={} det={}", a.file, b.file, a.exact && b.exact, a.det && b.det);
    }
    let d = tp as i64 - tb as i64;
    let _ = writeln!(
        md,
        "| **aggregate** | | {tb} | {tp} | {d:+} | {:+.3}% | {pb} | {pp} | {:.1}/{:.1} | {:.1}/{:.1} | {:.3}/{:.3} | {:.3}/{:.3} |",
        d as f64 / tb as f64 * 100.0,
        t[0] * 1e3,
        t[1] * 1e3,
        t[2] * 1e3,
        t[3] * 1e3,
        t[4],
        t[5],
        t[6],
        t[7]
    );
    let pct = |a: f64, b: f64| (b / a - 1.0) * 100.0;
    let _ = writeln!(
        md,
        "\nCompute cost of the candidate vs baseline (sum of best-of-{reps}): Stage 1 encode {:+.1}%, Stage 1 decode {:+.1}%, end-to-end encode {:+.1}%, end-to-end decode {:+.1}%.\nAll round trips pixel-exact and deterministic: {all_ok}.",
        pct(t[0], t[1]),
        pct(t[2], t[3]),
        pct(t[4], t[5]),
        pct(t[6], t[7])
    );
    std::fs::write(dir.join("report.md"), &md).unwrap();
    println!("{md}\nreport: {}", dir.join("report.md").display());
    if !all_ok {
        std::process::exit(1);
    }
}
