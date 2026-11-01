//! ELLIE Stage 4 coder-slack diagnostic (profile Bc255S11, bench50 corpus, 1024-px centre crops). Measurement only.
//!
//! A pass-through `BitIo` wrapper sits between Stage 3 and the unmodified production `Stage4Enc`. For every binary
//! decision it records -log2 P(actual bit) from the exact `p1` handed to the coder (post scaling and clamping), per
//! decision class, plus a shadow of the coder's 32-bit `range` (same integer arithmetic) so the coder's own interval cost
//! (8*renorm shifts + log2(2^32/range)) can be split from termination. Stage 1-3 code, Stage 4 and the container are untouched.
//!
//!   cargo run --release --bin bench_ellie_coder_slack -- --out <dir> [--set 3|50] [--reps N]
//! Writes <dir>/per_image.csv and <dir>/results.json.
use nanoscope::canon::{Canonical, Layout, Spec};
use nanoscope::ellie::{self, EllieCodec, Stage3Profile, Stage4Enc};
use nanoscope::pkon::{self, CodecParams, EllieRepr, ELLIE_REPR_YCOCG_R, FIXED_HEADER_LEN};
use nanoscope::rangecoder::BitIo;
use std::path::PathBuf;
use std::time::Instant;

const PROFILE: Stage3Profile = Stage3Profile::Bc255S11;
const CLASSES: [&str; 5] = ["zero_flag", "exponent_unary", "mantissa_modelled", "mantissa_raw", "sign"];

#[derive(Clone, Copy, PartialEq)]
enum Phase {
    Zero,
    Unary,
    Mant1,
    Mant2,
    Raw,
    Sign,
}

#[derive(Clone, Default)]
struct Totals {
    cost: [f64; 5],
    n: [u64; 5],
    ones: [u64; 5],
    /// 8 * renorm shifts + log2((2^32-1) / final range): the coder's own interval cost, before termination.
    interval_bits: f64,
    payload_bytes: u64,
    raw_p_violations: u64,
    /// Per plane stream: output bits minus the coder's interval bits (flush, byte alignment, dropped lead byte, trimmed zeros).
    tails: Vec<f64>,
    /// Per plane stream: output bits minus the interval bits rounded up to whole bytes.
    tails_over: Vec<f64>,
}

struct Meter {
    enc: Stage4Enc,
    tab: Vec<f64>,
    kmax: u32,
    phase: Phase,
    k: u32,
    raw_left: u32,
    range: u32,
    shifts: u64,
    t: Totals,
}

impl Meter {
    fn new(cap: usize) -> Self {
        // tab[p] = -log2(p / 65536); the cost of a bit whose probability is p/65536.
        let tab = (0..=65536usize).map(|p| if p == 0 { f64::INFINITY } else { -(p as f64 / 65536.0).log2() }).collect();
        Self { enc: Stage4Enc::with_capacity(cap), tab, kmax: 0, phase: Phase::Zero, k: 0, raw_left: 0, range: u32::MAX, shifts: 0, t: Totals::default() }
    }
    fn begin_plane(&mut self, bits: u32) {
        self.enc.reset();
        self.kmax = 32 - (2 * ((1u32 << bits) - 1)).leading_zeros();
        self.phase = Phase::Zero;
        self.range = u32::MAX;
        self.shifts = 0;
    }
    fn end_plane(&mut self) -> Vec<u8> {
        let interval = 8.0 * self.shifts as f64 + (u32::MAX as f64 / self.range as f64).log2();
        self.t.interval_bits += interval;
        let bytes = self.enc.finish().to_vec();
        self.t.payload_bytes += bytes.len() as u64;
        self.t.tails.push(8.0 * bytes.len() as f64 - interval);
        self.t.tails_over.push(8.0 * bytes.len() as f64 - 8.0 * (interval / 8.0).ceil());
        bytes
    }
}

impl BitIo for Meter {
    #[inline(always)]
    fn code(&mut self, p1: u32, bit: u32) -> u32 {
        let c = match self.phase {
            Phase::Zero => 0,
            Phase::Unary => 1,
            Phase::Mant1 | Phase::Mant2 => 2,
            Phase::Raw => 3,
            Phase::Sign => 4,
        };
        // P(bit = 1) = p1/65536, P(bit = 0) = (65536 - p1)/65536; both exact on the integer scale.
        self.t.cost[c] += self.tab[if bit != 0 { p1 } else { 65536 - p1 } as usize];
        self.t.n[c] += 1;
        self.t.ones[c] += bit as u64;
        if (c == 3) != (p1 == 32768) && c != 4 {
            self.t.raw_p_violations += 1; // a modelled bit can never be exactly 32768 (p16 = 16*p12 + 8); raw always is
        }
        // Decision-class state machine mirroring Stage3::code_sample_g.
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
            Phase::Mant1 => self.phase = if self.k >= 3 { Phase::Mant2 } else { Phase::Sign },
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
        // Shadow of Stage4Enc's range arithmetic.
        let bound = ((self.range as u64 * p1 as u64) >> 16) as u32;
        self.range = if bit != 0 { bound } else { self.range - bound };
        while self.range < (1 << 24) {
            self.range <<= 8;
            self.shifts += 1;
        }
        self.enc.code(p1, bit)
    }
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

/// Same plane split / YCoCg-R forward as pkon::encode_ellie (8-bit input only; the corpus is 8-bit RGB/RGBA).
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

/// Instrumented encode of the representation production chose; returns the concatenated plane streams and the meter totals.
fn encode_metered(planes: &[Vec<u16>], plane_bits: &[u8], w: usize, h: usize) -> (Vec<u8>, Totals) {
    let mut m = Meter::new(w * h);
    let mut codecs: Vec<(u8, EllieCodec)> = vec![];
    let mut payload = Vec::new();
    for (k, plane) in planes.iter().enumerate() {
        let bits = plane_bits[k];
        let at = match codecs.iter().position(|c| c.0 == bits) {
            Some(i) => i,
            None => {
                codecs.push((bits, EllieCodec::with_profile(w, h, bits as u32, PROFILE).unwrap()));
                codecs.len() - 1
            }
        };
        m.begin_plane(bits as u32);
        codecs[at].1.encode_planes(&[plane.as_slice()], &mut m).unwrap();
        payload.extend_from_slice(&m.end_plane());
    }
    (payload, m.t)
}

fn quantile(v: &[f64], q: f64) -> f64 {
    let mut s = v.to_vec();
    s.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let x = q * (s.len() - 1) as f64;
    let (lo, hi) = (x.floor() as usize, x.ceil() as usize);
    s[lo] + (s[hi] - s[lo]) * (x - lo as f64)
}

fn dist(v: &[f64]) -> serde_json::Value {
    serde_json::json!({"min": quantile(v, 0.0), "p10": quantile(v, 0.1), "p25": quantile(v, 0.25), "median": quantile(v, 0.5),
        "p75": quantile(v, 0.75), "p90": quantile(v, 0.9), "max": quantile(v, 1.0), "mean": v.iter().sum::<f64>() / v.len() as f64})
}

fn main() {
    if cfg!(debug_assertions) {
        eprintln!("use --release");
        std::process::exit(2);
    }
    let a: Vec<String> = std::env::args().collect();
    let arg = |k: &str| a.iter().position(|x| x == k).map(|i| a[i + 1].clone());
    let set = arg("--set").unwrap_or("50".into());
    let reps: usize = arg("--reps").unwrap_or("2".into()).parse().unwrap();
    assert!(reps >= 2, "determinism needs at least two instrumented encodes");
    let out = PathBuf::from(arg("--out").unwrap());
    let refdir = PathBuf::from(arg("--ref").unwrap_or("benchmarks/ellie-bc-integration/ab50/bc255s11".into()));
    std::fs::create_dir_all(&out).unwrap();

    let mut csv = String::from(
        "image,w,h,planes,repr,pixels,file_bytes,header_bytes,meta_bytes,payload_bytes,payload_bits,model_bits,interval_bits,arith_loss_bits,term_bits,slack_bits,slack_pct,payload_bpp,model_bpp,\
         zero_bits,exp_bits,mant_model_bits,mant_raw_bits,sign_bits,decisions,deterministic,payload_identical\n",
    );
    let mut agg = Totals::default();
    let (mut tot_file, mut tot_hdr, mut tot_meta, mut tot_px, mut tot_payload_bits) = (0u64, 0u64, 0u64, 0u64, 0u64);
    let (mut all_det, mut all_ident) = (true, true);
    let mut per_image = vec![];
    let (mut slack_bits_v, mut slack_pct_v, mut term_v, mut tail_v, mut tail_over_v) = (vec![], vec![], vec![], vec![], vec![]);
    for (name, img) in load(&set) {
        let (w, h) = (img.spec.width as usize, img.spec.height as usize);
        let px = (w * h) as u64;
        // The recovered production file: source of the representation, plane depths and the reference payload.
        let prod = std::fs::read(refdir.join(format!("{name}.pkon"))).unwrap();
        let hdr = pkon::parse_header(&prod).unwrap();
        let CodecParams::Ellie(p) = &hdr.params else { panic!("not ELLIE") };
        assert_eq!((hdr.width as usize, hdr.height as usize), (w, h), "{name}: recovered file dimensions differ from the corpus crop");
        let ycocg = p.representation == ELLIE_REPR_YCOCG_R;
        let plane_bits: Vec<u8> = p.planes.iter().map(|x| x.0).collect();
        let prod_payload = &prod[FIXED_HEADER_LEN + hdr.meta_len as usize..];
        assert_eq!(prod_payload.len() as u32, hdr.payload_len);
        let mut first: Option<(Vec<u8>, Totals)> = None;
        let mut det = true;
        for _ in 0..reps {
            let r = encode_metered(&split_planes(&img, ycocg), &plane_bits, w, h);
            match &first {
                None => first = Some(r),
                Some((b, t0)) => {
                    det &= *b == r.0 && t0.cost.iter().zip(&r.1.cost).all(|(x, y)| x.to_bits() == y.to_bits()) && t0.interval_bits.to_bits() == r.1.interval_bits.to_bits();
                }
            }
        }
        let (payload, t) = first.unwrap();
        let ident = payload == prod_payload;
        let model: f64 = t.cost.iter().sum();
        let actual = 8.0 * payload.len() as f64;
        let term = actual - t.interval_bits;
        let slack = actual - model;
        slack_bits_v.push(slack);
        slack_pct_v.push(slack / model * 100.0);
        term_v.push(term);
        tail_v.extend(t.tails.iter().copied());
        tail_over_v.extend(t.tails_over.iter().copied());
        println!("{name}: model {:.0} b  payload {} b  slack {:.0} b ({:.4}%)  ycocg={ycocg} det={det} ident={ident}", model, actual as u64, slack, slack / model * 100.0);
        csv += &format!(
            "{name},{w},{h},{},{},{px},{},{FIXED_HEADER_LEN},{},{},{},{:.3},{:.3},{:.3},{:.3},{:.3},{:.5},{:.5},{:.5},{:.3},{:.3},{:.3},{:.3},{:.3},{},{det},{ident}\n",
            plane_bits.len(), p.representation, prod.len(), hdr.meta_len, payload.len(), actual as u64, model, t.interval_bits, t.interval_bits - model, term, slack, slack / model * 100.0,
            actual / px as f64, model / px as f64, t.cost[0], t.cost[1], t.cost[2], t.cost[3], t.cost[4], t.n.iter().sum::<u64>()
        );
        per_image.push(serde_json::json!({"image": name, "repr": p.representation, "pixels": px, "file_bytes": prod.len(), "meta_bytes": hdr.meta_len,
            "payload_bits": actual, "model_bits": model, "interval_bits": t.interval_bits, "termination_bits": term, "slack_bits": slack, "slack_pct": slack / model * 100.0,
            "payload_bpp": actual / px as f64, "model_bpp": model / px as f64, "deterministic": det, "payload_identical": ident}));
        for c in 0..5 {
            agg.cost[c] += t.cost[c];
            agg.n[c] += t.n[c];
            agg.ones[c] += t.ones[c];
        }
        agg.interval_bits += t.interval_bits;
        agg.payload_bytes += payload.len() as u64;
        agg.raw_p_violations += t.raw_p_violations;
        tot_file += prod.len() as u64;
        tot_hdr += FIXED_HEADER_LEN as u64;
        tot_meta += hdr.meta_len as u64;
        tot_px += px;
        tot_payload_bits += payload.len() as u64 * 8;
        all_det &= det;
        all_ident &= ident;
    }
    let model: f64 = agg.cost.iter().sum();
    let actual = tot_payload_bits as f64;
    let classes: Vec<_> = (0..5)
        .map(|c| serde_json::json!({"class": CLASSES[c], "decisions": agg.n[c], "ones": agg.ones[c], "model_bits": agg.cost[c], "bits_per_decision": agg.cost[c] / agg.n[c] as f64}))
        .collect();
    let res = serde_json::json!({
        "profile": "Bc255S11", "set": set, "images": per_image.len(), "streams": tail_v.len(), "pixels": tot_px, "reps": reps, "reference_dir": refdir.to_string_lossy(),
        "file_bytes_total": tot_file, "header_bytes_total": tot_hdr, "meta_bytes_total": tot_meta, "container_bytes_total": tot_hdr + tot_meta,
        "payload_bytes_total": agg.payload_bytes, "payload_bits_total": tot_payload_bits,
        "model_bits_total": model, "model_bytes_total": model / 8.0,
        "interval_bits_total": agg.interval_bits, "arith_loss_bits": agg.interval_bits - model, "termination_bits": actual - agg.interval_bits,
        "slack_bits": actual - model, "slack_bytes": (actual - model) / 8.0, "slack_pct": (actual - model) / model * 100.0,
        "arith_loss_pct_of_model": (agg.interval_bits - model) / model * 100.0, "termination_pct_of_model": (actual - agg.interval_bits) / model * 100.0,
        "container_pct_of_file": (tot_hdr + tot_meta) as f64 / tot_file as f64 * 100.0,
        "payload_bpp": actual / tot_px as f64, "model_bpp": model / tot_px as f64,
        "dist_slack_bits_per_image": dist(&slack_bits_v), "dist_slack_pct_per_image": dist(&slack_pct_v), "dist_termination_bits_per_image": dist(&term_v),
        "dist_termination_bits_per_stream": dist(&tail_v), "dist_termination_over_byte_aligned_interval_bits_per_stream": dist(&tail_over_v),
        "classes": classes, "raw_p_violations": agg.raw_p_violations,
        "all_deterministic": all_det, "all_payload_identical": all_ident,
        "per_image": per_image,
    });
    std::fs::write(out.join("per_image.csv"), csv).unwrap();
    std::fs::write(out.join("results.json"), serde_json::to_string_pretty(&res).unwrap()).unwrap();
    println!("TOTAL files {tot_file} B  payload {} B  model {:.0} b ({:.0} B)  slack {:.0} b ({:.4}%)  det={all_det} ident={all_ident}", agg.payload_bytes, model, model / 8.0, actual - model, (actual - model) / model * 100.0);
    let _ = Layout::Rgb;
}
