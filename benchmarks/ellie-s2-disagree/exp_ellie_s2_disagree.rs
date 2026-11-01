//! ELLIE Stage 2 directional-disagreement experiment (profile Bc255S11, bench50 corpus, 1024-px centre crops).
//!
//! Q1 (`--mode analyze`): does the spread of Stage 1's own H/V/D estimates carry information about residual size, sign
//!     persistence, variance or bias staleness, beyond what the neighbouring residuals already say? Does the best bias
//!     learning rate differ by disagreement level (fit on even-indexed images, tested on odd-indexed ones)?
//! Q2 (`--mode bench`): through the real `.pkon` size accounting, does a disagreement-selected learning shift beat the
//!     fixed shift 6 baseline, at what time cost, and is it lossless/deterministic?
//!
//! Variants in `bench`: `prod` = unmodified `pkon::encode_standard_profile`; `ctrl` = this file's Stage 1 copy with
//! fixed shift 6 (must be byte-identical to prod; isolates harness effects); `exp` = per-sample shift from `--tab`.
//!   cargo run --release --bin exp_ellie_s2_disagree -- --mode analyze|bench --out <dir> [--set 3|50] [--reps N] [--tab 16 comma-separated shifts]
mod s1x;
use nanoscope::canon::{Canonical, Spec};
use nanoscope::ellie::{self, Stage3, Stage3Profile, Stage4Dec, Stage4Enc};
use nanoscope::pkon::{self, CodecParams, EllieRepr, ELLIE_REPR_YCOCG_R, FIXED_HEADER_LEN};
use s1x::Stage1X;
use std::path::PathBuf;
use std::time::Instant;

const PROFILE: Stage3Profile = Stage3Profile::Bc255S11;
const NB: usize = 16;
const SHIFTS: [u32; 5] = [4, 5, 6, 7, 8];

#[inline(always)]
fn bin(dis4: u32) -> usize {
    ((32 - dis4.leading_zeros()) as usize).min(NB - 1)
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

/// Same plane split / YCoCg-R forward as pkon::encode_ellie (8-bit RGB/RGBA only; the corpus is 8-bit).
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

fn plane_bits(ycocg: bool, ch: usize) -> Vec<u8> {
    (0..ch).map(|k| if ycocg && (k == 1 || k == 2) { ellie::YCOCG_CHROMA_BITS as u8 } else { 8 }).collect()
}

// ---------------------------------------------------------------- fused Stage 1 + Stage 2 (shift chosen per sample)

type Tab = [u8; NB];
const FIXED6: Tab = [6; NB];

/// Stage 2 as in production (S += ((e << 8) - S) >> K, B = (S + 128) >> 8) with K = tab[bin(disagreement)].
fn enc_plane(s1: &mut Stage1X, src: &[u16], res: &mut [i32], tab: &Tab) {
    let mut s = 0i32;
    s1.scan(|i, p, d| {
        let x = src[i];
        let e = x as i32 - p as i32;
        res[i] = e - ((s + 128) >> 8);
        s += ((e << 8) - s) >> tab[bin(d)];
        x as u32
    });
}

fn dec_plane(s1: &mut Stage1X, res: &[i32], out: &mut [u16], m: i32, tab: &Tab) -> bool {
    let mut s = 0i32;
    let mut bad = false;
    s1.scan(|i, p, d| {
        let e = res[i] + ((s + 128) >> 8);
        s += ((e << 8) - s) >> tab[bin(d)];
        let v = p as i32 + e;
        bad |= (v as u32) > m as u32;
        let v = v.clamp(0, m);
        out[i] = v as u16;
        v as u32
    });
    bad
}

struct Codec {
    s1: Vec<(u8, Stage1X)>,
    s3: Vec<(u8, Stage3)>,
    res: Vec<i32>,
    w: usize,
    h: usize,
}

impl Codec {
    fn new(w: usize, h: usize) -> Self {
        Self { s1: vec![], s3: vec![], res: vec![0; w * h], w, h }
    }
    fn get(&mut self, bits: u8) -> (&mut Stage1X, &mut Stage3, &mut Vec<i32>) {
        if !self.s1.iter().any(|c| c.0 == bits) {
            self.s1.push((bits, Stage1X::new(self.w, self.h, bits as u32).unwrap()));
            self.s3.push((bits, Stage3::with_profile(self.w, self.h, bits as u32, PROFILE).unwrap()));
        }
        let i = self.s1.iter().position(|c| c.0 == bits).unwrap();
        (&mut self.s1[i].1, &mut self.s3[i].1, &mut self.res)
    }
    /// One finished Stage 4 stream per plane.
    fn encode(&mut self, planes: &[Vec<u16>], pbits: &[u8], tab: &Tab) -> Vec<Vec<u8>> {
        let mut out = vec![];
        let mut enc = Stage4Enc::with_capacity(self.w * self.h);
        for (k, plane) in planes.iter().enumerate() {
            let (s1, s3, res) = self.get(pbits[k]);
            enc.reset();
            s3.reset();
            enc_plane(s1, plane, res, tab);
            s3.encode_plane(res, &mut enc).unwrap();
            out.push(enc.finish().to_vec());
        }
        out
    }
    fn decode(&mut self, streams: &[Vec<u8>], pbits: &[u8], tab: &Tab) -> Vec<Vec<u16>> {
        let mut planes = vec![];
        for (k, st) in streams.iter().enumerate() {
            let (s1, s3, res) = self.get(pbits[k]);
            let mut dec = Stage4Dec::new(st);
            s3.reset();
            s3.decode_plane(res, &mut dec).unwrap();
            let mut out = vec![0u16; res.len()];
            assert!(!dec_plane(s1, res, &mut out, (1i32 << pbits[k]) - 1, tab));
            planes.push(out);
        }
        planes
    }
}

struct Chosen {
    ycocg: bool,
    streams: Vec<Vec<u8>>,
}

impl Chosen {
    fn payload(&self) -> usize {
        self.streams.iter().map(|s| s.len()).sum()
    }
}

/// Production `Auto`: try both representations, keep YCoCg-R only if strictly smaller.
fn encode_auto(img: &Canonical, tab: &Tab) -> Chosen {
    let (w, h, ch) = (img.spec.width as usize, img.spec.height as usize, img.spec.layout.channels());
    let mut c = Codec::new(w, h);
    let plain = Chosen { ycocg: false, streams: c.encode(&split_planes(img, false), &plane_bits(false, ch), tab) };
    let yc = Chosen { ycocg: true, streams: c.encode(&split_planes(img, true), &plane_bits(true, ch), tab) };
    if yc.payload() < plain.payload() { yc } else { plain }
}

fn decode_auto(img_spec: &Spec, ch: &Chosen, tab: &Tab) -> Vec<u8> {
    let (w, h, c) = (img_spec.width as usize, img_spec.height as usize, img_spec.layout.channels());
    let mut codec = Codec::new(w, h);
    let mut planes = codec.decode(&ch.streams, &plane_bits(ch.ycocg, c), tab);
    if ch.ycocg {
        assert!(ellie::ycocg_r_inverse_planes(&mut planes[..3]));
    }
    let mut data = vec![0u8; w * h * c];
    for (k, p) in planes.iter().enumerate() {
        for (i, &v) in p.iter().enumerate() {
            data[i * c + k] = v as u8;
        }
    }
    data
}

fn parse_tab(s: &str) -> Tab {
    let v: Vec<u8> = s.split(',').map(|x| x.trim().parse().unwrap()).collect();
    assert_eq!(v.len(), NB);
    assert!(v.iter().all(|&x| (1..=10).contains(&x)));
    let mut t = [0u8; NB];
    t.copy_from_slice(&v);
    t
}

/// Representation production chose, read from the recovered production file.
fn ref_repr(refdir: &PathBuf, name: &str) -> bool {
    let prod = std::fs::read(refdir.join(format!("{name}.pkon"))).unwrap();
    let CodecParams::Ellie(p) = &pkon::parse_header(&prod).unwrap().params else { panic!() };
    p.representation == ELLIE_REPR_YCOCG_R
}

// ---------------------------------------------------------------- analysis

#[derive(Clone)]
struct Acc {
    n: [f64; NB],
    abs_e: [f64; NB],
    abs_r: [f64; NB],
    r2: [f64; NB],
    helps: [f64; NB],  // |r| < |e|
    hurts: [f64; NB],  // |r| > |e|
    pair_n: [f64; NB], // both r nonzero
    pair_same: [f64; NB],
    rr: [f64; NB], // sum r_i * r_{i-1}
    cost: [[f64; NB]; 5],
    // pooled Pearson sums over (x = log2(1+dis), y = log2(1+|r|), z = mean log2(1+|r|) of W and N)
    pc: [f64; 10],
}

impl Acc {
    fn new() -> Self {
        Self { n: [0.0; NB], abs_e: [0.0; NB], abs_r: [0.0; NB], r2: [0.0; NB], helps: [0.0; NB], hurts: [0.0; NB], pair_n: [0.0; NB], pair_same: [0.0; NB], rr: [0.0; NB], cost: [[0.0; NB]; 5], pc: [0.0; 10] }
    }
    fn add(&mut self, o: &Acc) {
        for b in 0..NB {
            self.n[b] += o.n[b];
            self.abs_e[b] += o.abs_e[b];
            self.abs_r[b] += o.abs_r[b];
            self.r2[b] += o.r2[b];
            self.helps[b] += o.helps[b];
            self.hurts[b] += o.hurts[b];
            self.pair_n[b] += o.pair_n[b];
            self.pair_same[b] += o.pair_same[b];
            self.rr[b] += o.rr[b];
            for k in 0..5 {
                self.cost[k][b] += o.cost[k][b];
            }
        }
        for i in 0..10 {
            self.pc[i] += o.pc[i];
        }
    }
}

fn analyze_plane(src: &[u16], bits: u8, w: usize, h: usize, lg: &[f32]) -> Acc {
    let mut s1 = Stage1X::new(w, h, bits as u32).unwrap();
    let n = w * h;
    let mut e = vec![0i32; n];
    let mut d = vec![0u32; n];
    s1.scan(|i, p, dis| {
        e[i] = src[i] as i32 - p as i32;
        d[i] = dis;
        src[i] as u32
    });
    let mut a = Acc::new();
    let l = |v: i32| lg[v.unsigned_abs().min(2047) as usize] as f64;
    // statistics of the production baseline residual r (shift 6)
    let mut r = vec![0i32; n];
    let mut s = 0i32;
    for i in 0..n {
        r[i] = e[i] - ((s + 128) >> 8);
        s += ((e[i] << 8) - s) >> 6;
    }
    for i in 0..n {
        let b = bin(d[i]);
        let (ei, ri) = (e[i], r[i]);
        a.n[b] += 1.0;
        a.abs_e[b] += ei.abs() as f64;
        a.abs_r[b] += ri.abs() as f64;
        a.r2[b] += (ri as f64) * (ri as f64);
        if ri.abs() < ei.abs() {
            a.helps[b] += 1.0;
        } else if ri.abs() > ei.abs() {
            a.hurts[b] += 1.0;
        }
        if i > 0 {
            a.rr[b] += ri as f64 * r[i - 1] as f64;
            if ri != 0 && r[i - 1] != 0 {
                a.pair_n[b] += 1.0;
                if (ri > 0) == (r[i - 1] > 0) {
                    a.pair_same[b] += 1.0;
                }
            }
        }
        if i >= w && i % w != 0 {
            let x = ((d[i] as f64) / 4.0 + 1.0).log2();
            let y = l(ri);
            let z = 0.5 * (l(r[i - 1]) + l(r[i - w]));
            let t = [1.0, x, y, z, x * x, y * y, z * z, x * y, x * z, y * z];
            for k in 0..10 {
                a.pc[k] += t[k];
            }
        }
    }
    // proxy cost under each fixed shift, attributed to the disagreement bin of the sample
    for (ki, &k) in SHIFTS.iter().enumerate() {
        let mut s = 0i32;
        for i in 0..n {
            let ri = e[i] - ((s + 128) >> 8);
            a.cost[ki][bin(d[i])] += lg[ri.unsigned_abs().min(2047) as usize] as f64;
            s += ((e[i] << 8) - s) >> k;
        }
    }
    a
}

fn pearson(pc: &[f64; 10], i: usize, j: usize, ii: usize, jj: usize, ij: usize) -> f64 {
    let n = pc[0];
    let (mi, mj) = (pc[i] / n, pc[j] / n);
    let cov = pc[ij] / n - mi * mj;
    cov / ((pc[ii] / n - mi * mi).sqrt() * (pc[jj] / n - mj * mj).sqrt())
}

fn run_analyze(set: &str, out: &PathBuf, refdir: &PathBuf) {
    let lg: Vec<f32> = (0..2048).map(|v| ((1 + v) as f32).log2()).collect();
    let (mut all, mut train, mut test) = (Acc::new(), Acc::new(), Acc::new());
    for (idx, (name, img)) in load(set).into_iter().enumerate() {
        let (w, h) = (img.spec.width as usize, img.spec.height as usize);
        let yc = ref_repr(refdir, &name);
        let planes = split_planes(&img, yc);
        let pb = plane_bits(yc, planes.len());
        let mut a = Acc::new();
        for (k, p) in planes.iter().enumerate() {
            a.add(&analyze_plane(p, pb[k], w, h, &lg));
        }
        all.add(&a);
        if idx % 2 == 0 { train.add(&a) } else { test.add(&a) }
        eprintln!("{name} analysed");
    }
    let mut txt = String::new();
    let mut p = |s: String| {
        println!("{s}");
        txt += &s;
        txt += "\n";
    };
    p("bin(bitlen dis4) | samples | share% | mean|e| | mean|r| | rms r | bias helps% | bias hurts% | sign-persist% | lag1 autocorr r".into());
    for b in 0..NB {
        if all.n[b] < 1.0 {
            continue;
        }
        let n = all.n[b];
        p(format!("{b:2} | {:.0} | {:.2} | {:.3} | {:.3} | {:.3} | {:.1} | {:.1} | {:.2} | {:.4}", n, 100.0 * n / all.n.iter().sum::<f64>(), all.abs_e[b] / n, all.abs_r[b] / n, (all.r2[b] / n).sqrt(),
            100.0 * all.helps[b] / n, 100.0 * all.hurts[b] / n, 100.0 * all.pair_same[b] / all.pair_n[b].max(1.0), all.rr[b] / all.r2[b].max(1.0)));
    }
    let pc = &all.pc;
    // indices: 1 x, 2 y, 3 z, 4 xx, 5 yy, 6 zz, 7 xy, 8 xz, 9 yz
    let (rxy, rxz, ryz) = (pearson(pc, 1, 2, 4, 5, 7), pearson(pc, 1, 3, 4, 6, 8), pearson(pc, 2, 3, 5, 6, 9));
    let partial = (rxy - rxz * ryz) / ((1.0 - rxz * rxz).sqrt() * (1.0 - ryz * ryz).sqrt());
    p(format!("\nPearson corr(log2 disagreement, log2(1+|r|)) = {rxy:.4}"));
    p(format!("Pearson corr(log2 disagreement, neighbour residual activity [W,N]) = {rxz:.4}"));
    p(format!("Pearson corr(log2(1+|r|), neighbour residual activity) = {ryz:.4}"));
    p(format!("Partial corr(disagreement, |r| | neighbour residual activity) = {partial:.4}"));
    p("\nproxy cost = sum log2(1+|r|) (all images), per fixed shift:".into());
    for (ki, &k) in SHIFTS.iter().enumerate() {
        let tot: f64 = all.cost[ki].iter().sum();
        p(format!("shift {k}: {:.0}  ({:+.4}% vs shift 6)", tot, 100.0 * (tot / all.cost[2].iter().sum::<f64>() - 1.0)));
    }
    p("\nper-bin best shift (train = even-indexed images, test = odd-indexed); cost change vs shift 6 within bin".into());
    let mut tab: Tab = FIXED6;
    let min_n = 1e5;
    for b in 0..NB {
        if train.n[b] < min_n {
            continue;
        }
        let best = (0..5).min_by(|&x, &y| train.cost[x][b].partial_cmp(&train.cost[y][b]).unwrap()).unwrap();
        tab[b] = SHIFTS[best] as u8;
        let g = |a: &Acc, k: usize| 100.0 * (a.cost[k][b] / a.cost[2][b] - 1.0);
        p(format!("bin {b:2}: train best shift {} (train {:+.3}%, test {:+.3}% vs 6)   per-shift test cost vs 6: {}", SHIFTS[best], g(&train, best), g(&test, best),
            (0..5).map(|k| format!("{}:{:+.3}%", SHIFTS[k], g(&test, k))).collect::<Vec<_>>().join(" ")));
    }
    let tab_cost = |a: &Acc| (0..NB).map(|b| a.cost[SHIFTS.iter().position(|&s| s == tab[b] as u32).unwrap()][b]).sum::<f64>();
    let base = |a: &Acc| a.cost[2].iter().sum::<f64>();
    p(format!("\ntable {:?}\nproxy change of table vs shift 6: train {:+.4}%  test {:+.4}%  (all {:+.4}%)", tab, 100.0 * (tab_cost(&train) / base(&train) - 1.0), 100.0 * (tab_cost(&test) / base(&test) - 1.0), 100.0 * (tab_cost(&all) / base(&all) - 1.0)));
    p(format!("tab arg: {}", tab.iter().map(|x| x.to_string()).collect::<Vec<_>>().join(",")));
    std::fs::write(out.join("analysis.txt"), txt).unwrap();
}

// ---------------------------------------------------------------- bench

fn run_bench(set: &str, out: &PathBuf, reps: usize, tab: &Tab) {
    assert!(reps >= 2);
    let mut csv = String::from("image,repr_prod,repr_exp,prod_bytes,ctrl_bytes,exp_bytes,delta_bytes,delta_pct,prod_enc_s,ctrl_enc_s,exp_enc_s,prod_dec_s,ctrl_dec_s,exp_dec_s,s12_ctrl_enc_s,s12_exp_enc_s,s12_ctrl_dec_s,s12_exp_dec_s,exact_prod,exact_ctrl,exact_exp,det_exp,ctrl_identical_prod\n");
    let (mut tp, mut tc, mut te) = (0u64, 0u64, 0u64);
    let mut tm = [0f64; 10];
    let (mut ok_exact, mut ok_det, mut ok_ident) = (true, true, true);
    let mut per = vec![];
    for (name, img) in load(set) {
        let ch = img.spec.layout.channels();
        let (mut t_enc, mut t_dec) = ([f64::MAX; 3], [f64::MAX; 3]);
        let mut prod_f = vec![];
        let (mut c_ch, mut e_ch): (Option<Chosen>, Option<Chosen>) = (None, None);
        let (mut ex, mut det) = ([true; 3], true);
        for rep in 0..reps {
            for j in 0..3 {
                let v = (j + rep) % 3;
                let t = Instant::now();
                let mut f = vec![];
                let mut chosen = None;
                match v {
                    0 => f = pkon::encode_standard_profile(&img, EllieRepr::Auto, PROFILE).unwrap(),
                    1 => chosen = Some(encode_auto(&img, &FIXED6)),
                    _ => chosen = Some(encode_auto(&img, tab)),
                }
                t_enc[v] = t_enc[v].min(t.elapsed().as_secs_f64());
                let t = Instant::now();
                let back: Vec<u8> = match v {
                    0 => match pkon::decode(&f).unwrap() {
                        pkon::Decoded::Standard(b) => b.data,
                        _ => panic!(),
                    },
                    1 => decode_auto(&img.spec, chosen.as_ref().unwrap(), &FIXED6),
                    _ => decode_auto(&img.spec, chosen.as_ref().unwrap(), tab),
                };
                t_dec[v] = t_dec[v].min(t.elapsed().as_secs_f64());
                ex[v] &= back == img.data;
                match v {
                    0 => {
                        if rep == 0 {
                            prod_f = f
                        } else {
                            det &= prod_f == f
                        }
                    }
                    1 => c_ch = chosen,
                    _ => {
                        if let Some(prev) = &e_ch {
                            det &= prev.ycocg == chosen.as_ref().unwrap().ycocg && prev.streams == chosen.as_ref().unwrap().streams;
                        }
                        e_ch = chosen;
                    }
                }
            }
        }
        let (c_ch, e_ch) = (c_ch.unwrap(), e_ch.unwrap());
        let hdr = pkon::parse_header(&prod_f).unwrap();
        let overhead = prod_f.len() - hdr.payload_len as usize;
        assert_eq!(overhead, FIXED_HEADER_LEN + hdr.meta_len as usize);
        let CodecParams::Ellie(pp) = &hdr.params else { panic!() };
        let prod_yc = pp.representation == ELLIE_REPR_YCOCG_R;
        let prod_payload = &prod_f[overhead..];
        let ident = c_ch.ycocg == prod_yc && c_ch.streams.concat() == prod_payload;
        let (pb, cb, eb) = (prod_f.len() as u64, (overhead + c_ch.payload()) as u64, (overhead + e_ch.payload()) as u64);
        // Stage 1 + 2 only (no Stage 3/4), on the planes of the representation each variant chose: best of 3.
        let s12 = |ycocg: bool, tab: &Tab| -> (f64, f64) {
            let (w, h) = (img.spec.width as usize, img.spec.height as usize);
            let planes = split_planes(&img, ycocg);
            let pbits = plane_bits(ycocg, ch);
            let mut c = Codec::new(w, h);
            let mut res: Vec<Vec<i32>> = planes.iter().map(|_| vec![0; w * h]).collect();
            let (mut be, mut bd) = (f64::MAX, f64::MAX);
            for _ in 0..3 {
                let t = Instant::now();
                for k in 0..planes.len() {
                    let (s1, _, _) = c.get(pbits[k]);
                    enc_plane(s1, &planes[k], &mut res[k], tab);
                }
                be = be.min(t.elapsed().as_secs_f64());
                let mut o = vec![0u16; w * h];
                let t = Instant::now();
                for k in 0..planes.len() {
                    let (s1, _, _) = c.get(pbits[k]);
                    assert!(!dec_plane(s1, &res[k], &mut o, (1 << pbits[k]) - 1, tab));
                }
                bd = bd.min(t.elapsed().as_secs_f64());
            }
            (be, bd)
        };
        let (ce, cd) = s12(c_ch.ycocg, &FIXED6);
        let (ee, ed) = s12(e_ch.ycocg, tab);
        let d = eb as i64 - pb as i64;
        let dp = 100.0 * d as f64 / pb as f64;
        println!("{name}: prod {pb}  ctrl {cb}  exp {eb}  ({:+.3}%)  ident={ident} exact={:?} det={det}", dp, ex);
        csv += &format!("{name},{},{},{pb},{cb},{eb},{d},{dp:.4},{:.5},{:.5},{:.5},{:.5},{:.5},{:.5},{ce:.5},{ee:.5},{cd:.5},{ed:.5},{},{},{},{det},{ident}\n",
            prod_yc as u8, e_ch.ycocg as u8, t_enc[0], t_enc[1], t_enc[2], t_dec[0], t_dec[1], t_dec[2], ex[0], ex[1], ex[2]);
        per.push(serde_json::json!({"image": name, "prod_bytes": pb, "ctrl_bytes": cb, "exp_bytes": eb, "delta_bytes": d, "delta_pct": dp}));
        tp += pb;
        tc += cb;
        te += eb;
        for (i, v) in [t_enc[0], t_enc[1], t_enc[2], t_dec[0], t_dec[1], t_dec[2], ce, ee, cd, ed].iter().enumerate() {
            tm[i] += v;
        }
        ok_exact &= ex.iter().all(|&x| x);
        ok_det &= det;
        ok_ident &= ident;
    }
    let res = serde_json::json!({
        "set": set, "reps": reps, "tab": tab.to_vec(), "prod_total": tp, "ctrl_total": tc, "exp_total": te,
        "delta_bytes": te as i64 - tp as i64, "delta_pct": 100.0 * (te as f64 - tp as f64) / tp as f64,
        "sum_best_seconds": {"enc": {"prod": tm[0], "ctrl": tm[1], "exp": tm[2]}, "dec": {"prod": tm[3], "ctrl": tm[4], "exp": tm[5]},
            "stage12_enc": {"ctrl": tm[6], "exp": tm[7]}, "stage12_dec": {"ctrl": tm[8], "exp": tm[9]}},
        "all_exact": ok_exact, "all_deterministic": ok_det, "ctrl_identical_to_prod": ok_ident, "per_image": per,
    });
    std::fs::write(out.join("per_image.csv"), csv).unwrap();
    std::fs::write(out.join("results.json"), serde_json::to_string_pretty(&res).unwrap()).unwrap();
    println!("TOTAL prod {tp}  ctrl {tc}  exp {te}  ({:+.4}%)  exact={ok_exact} det={ok_det} ctrl==prod={ok_ident}", 100.0 * (te as f64 - tp as f64) / tp as f64);
    println!("time(sum best s) enc prod/ctrl/exp {:.2}/{:.2}/{:.2}  dec {:.2}/{:.2}/{:.2}  stage1+2 enc ctrl/exp {:.3}/{:.3} dec {:.3}/{:.3}", tm[0], tm[1], tm[2], tm[3], tm[4], tm[5], tm[6], tm[7], tm[8], tm[9]);
}

fn main() {
    if cfg!(debug_assertions) {
        eprintln!("use --release");
        std::process::exit(2);
    }
    let a: Vec<String> = std::env::args().collect();
    let arg = |k: &str| a.iter().position(|x| x == k).map(|i| a[i + 1].clone());
    let set = arg("--set").unwrap_or("50".into());
    let out = PathBuf::from(arg("--out").unwrap());
    let refdir = PathBuf::from(arg("--ref").unwrap_or("benchmarks/ellie-bc-integration/ab50/bc255s11".into()));
    std::fs::create_dir_all(&out).unwrap();
    match arg("--mode").as_deref() {
        Some("analyze") => run_analyze(&set, &out, &refdir),
        Some("bench") => run_bench(&set, &out, arg("--reps").unwrap_or("2".into()).parse().unwrap(), &parse_tab(&arg("--tab").expect("--tab"))),
        _ => panic!("--mode analyze|bench"),
    }
}
