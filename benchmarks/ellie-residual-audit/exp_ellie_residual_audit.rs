//! ELLIE residual coding-efficiency audit (profile Bc255S11, three-image canary, 1024-px centre crops). Diagnostic only.
//!
//! Production Stage 1 (Paeth + Deja Vu Shrub p=2), Stage 2, Stage 3 (`Stage3::code_sample`, Bc255S11) and Stage 4 are
//! called unmodified. A pass-through `BitIo` meter sits between Stage 3 and `Stage4Enc`: it sees the exact `p1` and bit of
//! every decision. Stage 3's contexts are private, so the harness re-derives them from the public Stage 2 residual plane with
//! the production formulas (they are pure functions of already-coded residuals). The instrumented payload is checked byte for
//! byte against the live `pkon::encode_standard_with(Auto)` payload before any number is trusted.
//!
//!   cargo run --release --bin exp_ellie_residual_audit -- --out <dir>
use nanoscope::canon::{Canonical, Spec};
use nanoscope::ellie::{self, Stage1, Stage2, Stage3, Stage3Profile, Stage4Enc};
use nanoscope::pkon::{self, CodecParams, EllieRepr, ELLIE_REPR_YCOCG_R, FIXED_HEADER_LEN};
use nanoscope::rangecoder::BitIo;
use std::collections::BTreeMap;
use std::path::PathBuf;

const PROFILE: Stage3Profile = Stage3Profile::Bc255S11;
const CLASSES: [&str; 5] = ["zero_flag", "exponent_unary", "mantissa_modelled", "mantissa_raw", "sign"];
const NN: usize = 65; // nodes 0..62 modelled, 63 raw mantissa, 64 sign
const NQ: usize = 32;
const ND: usize = 6;
const NW: usize = 8;

fn q(v: u32) -> usize {
    if v < 4 {
        v as usize
    } else {
        let bl = 32 - v.leading_zeros();
        (2 * bl + ((v >> (bl - 2)) & 1) - 2) as usize
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Phase {
    Zero,
    Unary,
    Mant1,
    Mant2,
    Raw,
    Sign,
}

/// A partition of the decisions by (node, key). Tracks counts and the adaptive KT code length per class (learning cost included).
struct Part {
    cnt: Vec<[u32; 2]>,
    kt: [f64; 5],
}
impl Part {
    fn new(nkeys: usize) -> Self {
        Self { cnt: vec![[0; 2]; nkeys * NN], kt: [0.0; 5] }
    }
    #[inline]
    fn add(&mut self, key: usize, node: usize, cls: usize, bit: u32) {
        let c = &mut self.cnt[key * NN + node];
        let (n0, n1) = (c[0] as f64, c[1] as f64);
        let nb = if bit != 0 { n1 } else { n0 };
        self.kt[cls] -= ((nb + 0.5) / (n0 + n1 + 1.0)).log2();
        c[bit as usize] += 1;
    }
    /// Empirical conditional entropy (static, no learning cost: optimistic for sparse partitions) per class.
    fn static_bits(&self) -> [f64; 5] {
        let mut out = [0.0; 5];
        for (i, c) in self.cnt.iter().enumerate() {
            out[node_class(i % NN)] += hbits(c[0] as f64, c[1] as f64);
        }
        out
    }
}
fn hbits(n0: f64, n1: f64) -> f64 {
    let n = n0 + n1;
    let f = |x: f64| if x > 0.0 { -x * (x / n).log2() } else { 0.0 };
    f(n0) + f(n1)
}
fn node_class(node: usize) -> usize {
    match node {
        0 => 0,
        1..=15 => 1,
        63 => 3,
        64 => 4,
        _ => 2,
    }
}

struct Meter {
    enc: Stage4Enc,
    tab: Vec<f64>,
    kmax: u32,
    phase: Phase,
    k: u32,
    t: u32,
    raw_left: u32,
    range: u32,
    shifts: u64,
    ctx: [usize; 5], // qs, qd, qe, qw, sgi
    cost: [f64; 5],
    n: [u64; 5],
    ones: [u64; 5],
    parts: Vec<Part>,
    // per-qs production calibration: [qs][class] -> (n, cost, sum p(bit=1), ones)
    cal: Vec<[[f64; 4]; 5]>,
    node_cost: Vec<f64>,
}
impl Meter {
    fn new(cap: usize, bits: u32) -> Self {
        let tab = (0..=65536usize).map(|p| if p == 0 { f64::INFINITY } else { -(p as f64 / 65536.0).log2() }).collect();
        // order0(node), qs, B(qs,qd), C(qe,qw), B+qw, B+qe, B+qe+qw, sign(sW,sN)
        let sizes = [1, NQ, NQ * ND, NQ * NW, NQ * ND * NW, NQ * ND * NQ, NQ * ND * NQ * NW, 9];
        Self {
            enc: Stage4Enc::with_capacity(cap),
            tab,
            kmax: 32 - (2 * ((1u32 << bits) - 1)).leading_zeros(),
            phase: Phase::Zero,
            k: 0,
            t: 0,
            raw_left: 0,
            range: u32::MAX,
            shifts: 0,
            ctx: [0; 5],
            cost: [0.0; 5],
            n: [0; 5],
            ones: [0; 5],
            parts: sizes.iter().map(|&s| Part::new(s)).collect(),
            cal: vec![[[0.0; 4]; 5]; NQ],
            node_cost: vec![0.0; NN],
        }
    }
}
impl BitIo for Meter {
    #[inline(always)]
    fn code(&mut self, p1: u32, bit: u32) -> u32 {
        let (node, cls) = match self.phase {
            Phase::Zero => (0, 0),
            Phase::Unary => (self.k as usize, 1),
            Phase::Mant1 => (15 + self.k as usize, 2),
            Phase::Mant2 => (33 + 2 * (self.k as usize - 3) + self.t as usize, 2),
            Phase::Raw => (63, 3),
            Phase::Sign => (64, 4),
        };
        let c = self.tab[if bit != 0 { p1 } else { 65536 - p1 } as usize];
        self.cost[cls] += c;
        self.n[cls] += 1;
        self.ones[cls] += bit as u64;
        self.node_cost[node] += c;
        let [qs, qd, qe, qw, sg] = self.ctx;
        let cal = &mut self.cal[qs][cls];
        cal[0] += 1.0;
        cal[1] += c;
        cal[2] += p1 as f64 / 65536.0;
        cal[3] += bit as f64;
        let keys = [0, qs, qs * ND + qd, qe * NW + qw, (qs * ND + qd) * NW + qw, (qs * ND + qd) * NQ + qe, ((qs * ND + qd) * NQ + qe) * NW + qw, sg];
        for (p, &k) in self.parts.iter_mut().zip(&keys) {
            p.add(k, node, cls, bit);
        }
        match self.phase {
            Phase::Zero => {
                if bit != 0 {
                    self.phase = Phase::Unary;
                    self.k = 1;
                }
            }
            Phase::Unary => {
                let done = if bit != 0 {
                    self.k += 1;
                    self.k >= self.kmax
                } else {
                    true
                };
                if done {
                    self.phase = if self.k >= 2 { Phase::Mant1 } else { Phase::Sign };
                }
            }
            Phase::Mant1 => {
                self.t = bit;
                self.phase = if self.k >= 3 { Phase::Mant2 } else { Phase::Sign };
            }
            Phase::Mant2 => {
                self.raw_left = self.k - 3;
                self.phase = if self.raw_left > 0 { Phase::Raw } else { Phase::Sign };
            }
            Phase::Raw => {
                self.raw_left -= 1;
                if self.raw_left == 0 {
                    self.phase = Phase::Sign;
                }
            }
            Phase::Sign => self.phase = Phase::Zero,
        }
        let bound = ((self.range as u64 * p1 as u64) >> 16) as u32;
        self.range = if bit != 0 { bound } else { self.range - bound };
        while self.range < (1 << 24) {
            self.range <<= 8;
            self.shifts += 1;
        }
        self.enc.code(p1, bit)
    }
}

/// Cost-only sink (no coder): sums -log2 P(actual bit) from the exact p1 Stage 3 hands over. Used for the Stage 2 counterfactual.
struct CostIo {
    tab: Vec<f64>,
    cost: f64,
}
impl BitIo for CostIo {
    #[inline(always)]
    fn code(&mut self, p1: u32, bit: u32) -> u32 {
        self.cost += self.tab[if bit != 0 { p1 } else { 65536 - p1 } as usize];
        bit
    }
}

// ---------------------------------------------------------------- loading (same crop / plane split as the recent canaries)
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

// ---------------------------------------------------------------- residual statistics
type M = BTreeMap<String, f64>;
fn put(m: &mut M, k: &str, v: f64) {
    *m.entry(k.to_string()).or_insert(0.0) += v;
}
fn entropy_hist(h: &BTreeMap<i32, u64>) -> f64 {
    let n: u64 = h.values().sum();
    h.values().map(|&c| -(c as f64) * (c as f64 / n as f64).log2()).sum()
}

/// Additive moments + histograms of one residual plane. `tag` is "e" (Stage 1) or "r" (Stage 2). Everything is additive
/// across planes/images except min/max, so aggregates are formed by summing and deriving ratios afterwards.
fn stats(m: &mut M, tag: &str, v: &[i32], w: usize, hists: &mut Vec<(String, BTreeMap<i32, u64>)>, plane_id: &str) {
    let n = v.len() as f64;
    let (mut s1, mut s2, mut sa, mut z, mut mn, mut mx) = (0f64, 0f64, 0f64, 0f64, i32::MAX, i32::MIN);
    let mut hs: BTreeMap<i32, u64> = BTreeMap::new();
    let mut hm: BTreeMap<i32, u64> = BTreeMap::new();
    let (mut pos, mut neg) = (0f64, 0f64);
    let mut klog = [0f64; 20]; // magnitude bit-length distribution
    for &x in v {
        s1 += x as f64;
        s2 += (x as f64) * (x as f64);
        sa += x.abs() as f64;
        z += (x == 0) as u8 as f64;
        mn = mn.min(x);
        mx = mx.max(x);
        *hs.entry(x).or_default() += 1;
        *hm.entry(x.abs()).or_default() += 1;
        pos += (x > 0) as u8 as f64;
        neg += (x < 0) as u8 as f64;
        klog[(32 - x.unsigned_abs().leading_zeros()) as usize] += 1.0;
    }
    put(m, &format!("{tag}.n"), n);
    put(m, &format!("{tag}.sum"), s1);
    put(m, &format!("{tag}.sumsq"), s2);
    put(m, &format!("{tag}.sumabs"), sa);
    put(m, &format!("{tag}.zeros"), z);
    put(m, &format!("{tag}.pos"), pos);
    put(m, &format!("{tag}.neg"), neg);
    m.insert(format!("{tag}.min"), mn as f64);
    m.insert(format!("{tag}.max"), mx as f64);
    put(m, &format!("{tag}.H0_signed_bits"), entropy_hist(&hs));
    put(m, &format!("{tag}.H0_mag_bits"), entropy_hist(&hm));
    put(m, &format!("{tag}.H_sign_bits"), hbits(pos, neg)); // over non-zero samples
    for (k, c) in klog.iter().enumerate() {
        put(m, &format!("{tag}.klog{k:02}"), *c);
    }
    // Pearson autocorrelation of the signed residual and of |residual| at short causal lags, pooled by overlap count.
    let h = v.len() / w;
    let mean = s1 / n;
    let ma = sa / n;
    let var = s2 / n - mean * mean;
    let vara = (v.iter().map(|&x| (x.abs() as f64).powi(2)).sum::<f64>()) / n - ma * ma;
    for &(dx, dy) in &[(1i32, 0i32), (2, 0), (3, 0), (0, 1), (0, 2), (1, 1), (-1, 1), (2, 1), (-2, 1)] {
        let (mut cs, mut ca, mut cnt) = (0f64, 0f64, 0f64);
        for y in dy as usize..h {
            for x in 0..w {
                let xx = x as i32 - dx;
                if xx < 0 || xx >= w as i32 {
                    continue;
                }
                let a = v[y * w + x];
                let b = v[(y - dy as usize) * w + xx as usize];
                cs += (a as f64 - mean) * (b as f64 - mean);
                ca += ((a.abs() as f64) - ma) * ((b.abs() as f64) - ma);
                cnt += 1.0;
            }
        }
        put(m, &format!("{tag}.acf_signed.{dx}.{dy}"), if var > 0.0 { cs / var } else { 0.0 });
        put(m, &format!("{tag}.acf_abs.{dx}.{dy}"), if vara > 0.0 { ca / vara } else { 0.0 });
        put(m, &format!("{tag}.acf_cnt.{dx}.{dy}"), cnt);
    }
    hists.push((format!("{plane_id},{tag}_signed"), hs));
    hists.push((format!("{plane_id},{tag}_mag"), hm));
}

/// Re-derive Stage 3's context inputs for sample (x, y) from the Stage 2 residual plane, exactly as `code_sample_g` does
/// (row-end pads replicate the end of the row above; the left pad of W is |N|; row 0 sees an all-zero previous row).
fn contexts(r: &[i32], w: usize, x: usize, y: usize, e: i32) -> [usize; 5] {
    let a = |yy: usize, xx: usize| r[yy * w + xx].unsigned_abs();
    let s = |v: i32| if v == 0 { 1usize } else if v > 0 { 2 } else { 0 };
    let nrow = |xx: i32| -> u32 {
        if y == 0 {
            return 0;
        }
        a(y - 1, xx.clamp(0, w as i32 - 1) as usize)
    };
    let an = nrow(x as i32);
    let aw = if x == 0 { an } else { a(y, x - 1) };
    let anw = nrow(x as i32 - 1);
    let ane = nrow(x as i32 + 1);
    let sum = 2 * aw + 2 * an + anw + ane;
    let qs = q(sum).min(NQ - 1);
    let qd = q(aw.abs_diff(an)).min(ND - 1);
    let qe = q((e >> 2) as u32).min(NQ - 1);
    let qw = q(aw).min(NW - 1);
    let sw = if x == 0 { 1 } else { s(r[y * w + x - 1]) };
    let sn = if y == 0 { 1 } else { s(r[(y - 1) * w + x]) };
    [qs, qd, qe, qw, sw * 3 + sn]
}

fn accumulate(into: &mut M, from: &M) {
    for (kk, v) in from {
        if kk.ends_with(".min") {
            let c = into.entry(kk.clone()).or_insert(f64::MAX);
            *c = c.min(*v);
        } else if kk.ends_with(".max") {
            let c = into.entry(kk.clone()).or_insert(f64::MIN);
            *c = c.max(*v);
        } else {
            *into.entry(kk.clone()).or_insert(0.0) += v;
        }
    }
}

fn main() {
    if cfg!(debug_assertions) {
        eprintln!("use --release");
        std::process::exit(2);
    }
    let a: Vec<String> = std::env::args().collect();
    let out = PathBuf::from(a.iter().position(|x| x == "--out").map(|i| a[i + 1].clone()).expect("--out <dir>"));
    std::fs::create_dir_all(&out).unwrap();
    let data = PathBuf::from("data");
    let images: Vec<(String, Canonical)> = ["portrait_0.jpg", "screen_0.png", "night_0.jpg"]
        .iter()
        .map(|f| (f.split('.').next().unwrap().to_string(), center_crop(&Canonical::load(&data.join(f)).unwrap(), 1024)))
        .collect();

    let mut rows: Vec<(String, String, M)> = vec![]; // (image, plane, metrics); plane "ALL" = image sum
    let mut hists: Vec<(String, BTreeMap<i32, u64>)> = vec![];
    let mut qs_rows = String::from("image,plane,qs,class,decisions,model_bits,mean_p1_pred,obs_freq1\n");
    let mut log = String::new();
    for (name, img) in &images {
        let (w, h) = (img.spec.width as usize, img.spec.height as usize);
        // Live production file: chooses the representation (Auto) and provides the reference payload.
        let prod = pkon::encode_standard_with(img, EllieRepr::Auto).unwrap();
        let hdr = pkon::parse_header(&prod).unwrap();
        let CodecParams::Ellie(p) = &hdr.params else { panic!("not ELLIE") };
        let ycocg = p.representation == ELLIE_REPR_YCOCG_R;
        let plane_bits: Vec<u8> = p.planes.iter().map(|x| x.0).collect();
        let prod_payload = &prod[FIXED_HEADER_LEN + hdr.meta_len as usize..];
        let planes = split_planes(img, ycocg);
        let mut payload = vec![];
        let mut s1_8 = Stage1::new(w, h, 8).unwrap();
        let mut s1_9 = Stage1::new(w, h, 9).unwrap();
        let first = rows.len();
        for (k, plane) in planes.iter().enumerate() {
            let bits = plane_bits[k] as u32;
            let pid = format!("{name},{k}");
            let mut m = M::new();
            // Stage 1 (production) -> e;  Stage 2 (production) -> r
            let mut e = vec![0i32; w * h];
            let mut pred = vec![0u16; w * h];
            (if bits == 8 { &mut s1_8 } else { &mut s1_9 }).encode_with_pred(plane, &mut e, &mut pred).unwrap();
            let mut r = e.clone();
            Stage2::new().encode_in_place(&mut r);
            stats(&mut m, "e", &e, w, &mut hists, &pid);
            stats(&mut m, "r", &r, w, &mut hists, &pid);
            // Stage 3 (production code_sample) through the meter into Stage 4 (production)
            let mut s3 = Stage3::with_profile(w, h, bits, PROFILE).unwrap();
            let mut mt = Meter::new(w * h, bits);
            let mut ee = 0i32;
            for y in 0..h {
                for x in 0..w {
                    let i = y * w + x;
                    mt.ctx = contexts(&r, w, x, y, ee);
                    mt.phase = Phase::Zero;
                    s3.code_sample(&mut mt, r[i]);
                    ee += (((r[i].unsigned_abs() as i32) << 4) - ee) >> 4;
                }
            }
            let interval = 8.0 * mt.shifts as f64 + (u32::MAX as f64 / mt.range as f64).log2();
            let bytes = mt.enc.finish().to_vec();
            let actual = 8.0 * bytes.len() as f64;
            payload.extend_from_slice(&bytes);
            let model: f64 = mt.cost.iter().sum();
            // Counterfactual (NOT production): the same production Stage 3 fed the Stage 1 residuals e, i.e. Stage 2 bypassed.
            let mut s3e = Stage3::with_profile(w, h, bits, PROFILE).unwrap();
            let mut cio = CostIo { tab: (0..=65536usize).map(|p| if p == 0 { f64::INFINITY } else { -(p as f64 / 65536.0).log2() }).collect(), cost: 0.0 };
            for &v in &e {
                s3e.code_sample(&mut cio, v);
            }
            put(&mut m, "cf_no_stage2.model_bits", cio.cost);
            put(&mut m, "px", (w * h) as f64);
            put(&mut m, "payload_bits", actual);
            put(&mut m, "model_bits", model);
            put(&mut m, "interval_bits", interval);
            for c in 0..5 {
                put(&mut m, &format!("cls{c}.n"), mt.n[c] as f64);
                put(&mut m, &format!("cls{c}.ones"), mt.ones[c] as f64);
                put(&mut m, &format!("cls{c}.model"), mt.cost[c]);
            }
            for (pi, part) in mt.parts.iter().enumerate() {
                let st = part.static_bits();
                for c in 0..5 {
                    put(&mut m, &format!("part{pi}.kt.{c}"), part.kt[c]);
                    put(&mut m, &format!("part{pi}.static.{c}"), st[c]);
                }
            }
            for qsv in 0..NQ {
                for c in 0..5 {
                    let cal = mt.cal[qsv][c];
                    if cal[0] > 0.0 {
                        qs_rows += &format!("{name},{k},{qsv},{},{},{:.3},{:.6},{:.6}\n", CLASSES[c], cal[0], cal[1], cal[2] / cal[0], cal[3] / cal[0]);
                    }
                    put(&mut m, &format!("qs{qsv:02}.n.{c}"), cal[0]);
                    put(&mut m, &format!("qs{qsv:02}.model.{c}"), cal[1]);
                    put(&mut m, &format!("qs{qsv:02}.psum.{c}"), cal[2]);
                    put(&mut m, &format!("qs{qsv:02}.ones.{c}"), cal[3]);
                }
            }
            for nd in 0..NN {
                put(&mut m, &format!("node{nd:02}.model"), mt.node_cost[nd]);
            }
            log += &format!("{name} plane {k} ({bits}b): n={} model={:.0} interval={:.0} payload={:.0} bits\n", w * h, model, interval, actual);
            rows.push((name.clone(), k.to_string(), m));
        }
        let ident = payload == prod_payload;
        let mut tot = M::new();
        for (_, _, m) in &rows[first..] {
            accumulate(&mut tot, m);
        }
        tot.insert("file_bytes".into(), prod.len() as f64);
        tot.insert("header_bytes".into(), FIXED_HEADER_LEN as f64);
        tot.insert("meta_bytes".into(), hdr.meta_len as f64);
        tot.insert("ycocg".into(), ycocg as u8 as f64);
        println!("{name}: ycocg={ycocg} payload_identical_to_production={ident} file={}B", prod.len());
        assert!(ident, "{name}: instrumented payload differs from production");
        rows.push((name.clone(), "ALL".into(), tot));
    }
    let mut agg = M::new();
    for (_, _, m) in rows.iter().filter(|r| r.1 == "ALL") {
        accumulate(&mut agg, m);
    }
    rows.push(("AGGREGATE".into(), "ALL".into(), agg));

    let mut csv = String::from("image,plane,key,value\n");
    for (i, p, m) in &rows {
        for (k, v) in m {
            csv += &format!("{i},{p},{k},{v}\n");
        }
    }
    std::fs::write(out.join("metrics_long.csv"), csv).unwrap();
    std::fs::write(out.join("per_qs_calibration.csv"), qs_rows).unwrap();
    let mut hc = String::from("image,plane,series,value,count\n");
    for (id, hm) in &hists {
        for (v, c) in hm {
            hc += &format!("{id},{v},{c}\n");
        }
    }
    std::fs::write(out.join("histograms.csv"), hc).unwrap();
    std::fs::write(out.join("run.log"), log).unwrap();
    println!("wrote {}", out.display());
}
