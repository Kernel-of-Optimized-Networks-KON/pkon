//! (Paeth + YCoCg-R rerun; copy of bench_ellie_full.rs with only the Ellie codec changed) ELLIE vs the established 50-image benchmark (bench50): same dataset (data/dataset50.json), same raster (1024-px centre
//! crop of the decoded image, RGB8), same competitors/configs (jxl-e7-t1, jxl-e9-t1, webp-m6-t1, png-rs), same timing
//! (round_trip, best of 2, ffmpeg process floor reported separately). The only change from bench50.rs: the `pkon2-file`
//! codec is replaced by `ellie` (Stages 1-4, headerless payload, w/h/layout out-of-band like the earlier pkon2 reports).
//! The ffmpeg/PNG harness below is copied unchanged from bench50.rs.
//!
//!   cargo run --release --bin bench_ellie_full -- --smoke
//!   cargo run --release --bin bench_ellie_full
use nanoscope::canon::{first_mismatch, Canonical, Layout, Spec};
use nanoscope::codec::{Codec, CodecError, PngCodec};
use nanoscope::ellie::{EllieCodec, Stage4Enc};
use nanoscope::rangecoder::BitIo;
use nanoscope::verify::{round_trip, truncation_check, Verdict};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const CROP: u32 = 1024;
const REPS: u32 = 2;


// ---------------------------------------------------------------- ffmpeg-driven JXL / WebP

#[derive(Clone, Copy)]
enum Kind {
    Jxl(u8),
    Webp(u8),
}
struct Ff {
    kind: Kind,
    /// 0 = library default (all cores); n = `-threads n`
    threads: u32,
}
impl Ff {
    fn fmt(spec: &Spec) -> Result<&'static str, CodecError> {
        match (spec.layout, spec.bit_depth) {
            (Layout::Rgb, 8) => Ok("rgb24"),
            (Layout::Rgba, 8) => Ok("rgba"),
            _ => Err(CodecError::Unsupported(spec.label())),
        }
    }
    fn run(&self, args: &[String]) -> Result<(), CodecError> {
        let mut child = Command::new("ffmpeg")
            .args(["-hide_banner", "-loglevel", "error", "-xerror", "-y", "-nostdin"])
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| CodecError::Failed(e.to_string()))?;
        let deadline = Instant::now() + Duration::from_secs(240);
        loop {
            match child.try_wait() {
                Ok(Some(_)) => break,
                Ok(None) if Instant::now() > deadline => {
                    let _ = child.kill();
                    return Err(CodecError::Failed("ffmpeg timeout".into()));
                }
                Ok(None) => std::thread::sleep(Duration::from_millis(1)),
                Err(e) => return Err(CodecError::Failed(e.to_string())),
            }
        }
        let out = child.wait_with_output().map_err(|e| CodecError::Failed(e.to_string()))?;
        if out.status.success() {
            Ok(())
        } else {
            Err(CodecError::Failed(String::from_utf8_lossy(&out.stderr).lines().last().unwrap_or("ffmpeg failed").into()))
        }
    }
    fn ext(&self) -> &'static str {
        match self.kind {
            Kind::Jxl(_) => "jxl",
            Kind::Webp(_) => "webp",
        }
    }
}
struct Tmp(PathBuf);
impl Tmp {
    fn new() -> Self {
        static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let p = std::env::temp_dir().join(format!("nsx-{}-{}", std::process::id(), N.fetch_add(1, std::sync::atomic::Ordering::Relaxed)));
        std::fs::create_dir_all(&p).unwrap();
        Tmp(p)
    }
}
impl Drop for Tmp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
impl Codec for Ff {
    fn name(&self) -> String {
        let t = if self.threads == 0 { "mt".to_string() } else { format!("t{}", self.threads) };
        match self.kind {
            Kind::Jxl(e) => format!("jxl-e{e}-{t}"),
            Kind::Webp(l) => format!("webp-m{l}-{t}"),
        }
    }
    fn version(&self) -> String {
        String::new()
    }
    fn config(&self) -> String {
        String::new()
    }
    fn encode(&self, img: &Canonical) -> Result<Vec<u8>, CodecError> {
        let fmt = Ff::fmt(&img.spec)?;
        let tmp = Tmp::new();
        let (inp, outp) = (tmp.0.join("in.raw"), tmp.0.join(format!("o.{}", self.ext())));
        std::fs::write(&inp, &img.data).map_err(|e| CodecError::Failed(e.to_string()))?;
        let s = |x: &str| x.to_string();
        let mut a = vec![s("-f"), s("rawvideo"), s("-pix_fmt"), s(fmt), s("-video_size"), format!("{}x{}", img.spec.width, img.spec.height), s("-i"), inp.to_string_lossy().into(), s("-frames:v"), s("1")];
        match self.kind {
            Kind::Jxl(e) => a.extend([s("-c:v"), s("libjxl"), s("-distance"), s("0"), s("-effort"), e.to_string(), s("-pix_fmt"), s(fmt)]),
            // libwebp lossless takes bgra; rgb24 -> bgra with alpha=255 is exact (verified by the decode compare)
            Kind::Webp(l) => a.extend([s("-c:v"), s("libwebp"), s("-lossless"), s("1"), s("-compression_level"), l.to_string(), s("-quality"), s("100"), s("-pix_fmt"), s("bgra")]),
        }
        if self.threads > 0 {
            a.extend([s("-threads"), self.threads.to_string()]);
        }
        a.extend([s("-update"), s("1"), s("-f"), s("image2"), outp.to_string_lossy().into()]);
        self.run(&a)?;
        std::fs::read(&outp).map_err(|e| CodecError::Failed(e.to_string()))
    }
    fn decode(&self, enc: &[u8], hint: &Spec) -> Result<Canonical, CodecError> {
        let fmt = Ff::fmt(hint)?;
        let tmp = Tmp::new();
        let (inp, outp) = (tmp.0.join(format!("i.{}", self.ext())), tmp.0.join("out.raw"));
        std::fs::write(&inp, enc).map_err(|e| CodecError::Failed(e.to_string()))?;
        let s = |x: &str| x.to_string();
        let mut a = vec![];
        if self.threads > 0 {
            a.extend([s("-threads"), self.threads.to_string()]);
        }
        a.extend([s("-i"), inp.to_string_lossy().into(), s("-frames:v"), s("1"), s("-f"), s("rawvideo"), s("-pix_fmt"), s(fmt), outp.to_string_lossy().into()]);
        self.run(&a)?;
        Canonical::new(*hint, std::fs::read(&outp).map_err(|e| CodecError::Failed(e.to_string()))?).map_err(CodecError::Failed)
    }
}

/// Bytes that are container framing rather than codec payload.
fn overhead(codec: &str, enc: &[u8]) -> Option<u64> {
    if codec.starts_with("jxl") {
        // bare codestream (ff 0a signature) has no container box
        return if enc.starts_with(&[0xff, 0x0a]) { Some(0) } else { None };
    }
    if codec.starts_with("webp") {
        // RIFF(12) + chunk header(8) per chunk; payload = VP8L chunk data
        if enc.len() < 20 || &enc[0..4] != b"RIFF" || &enc[8..12] != b"WEBP" {
            return None;
        }
        let (mut pos, mut payload) = (12usize, 0u64);
        while pos + 8 <= enc.len() {
            let len = u32::from_le_bytes(enc[pos + 4..pos + 8].try_into().ok()?) as usize;
            if &enc[pos..pos + 4] == b"VP8L" {
                payload += len as u64;
            }
            pos += 8 + len + (len & 1);
        }
        return Some(enc.len() as u64 - payload);
    }
    if codec.starts_with("png") {
        return PngCodec.container_overhead(enc);
    }
    None
}


// ---------------------------------------------------------------- ELLIE (Stages 1-4)

/// Exact bench50 crop (copied from the removed pkon2::center_crop).
fn center_crop(img: &Canonical, max: u32) -> Canonical {
    let (w, h) = (img.spec.width, img.spec.height);
    if max == 0 || (w <= max && h <= max) {
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

fn split_planes(img: &Canonical) -> Vec<Vec<u16>> {
    let (w, h, ch) = (img.spec.width as usize, img.spec.height as usize, img.spec.layout.channels());
    let mut planes = vec![vec![0u16; w * h]; ch];
    for (i, px) in img.data.chunks_exact(ch).enumerate() {
        for (k, &v) in px.iter().enumerate() {
            planes[k][i] = v as u16;
        }
    }
    planes
}

/// Production ELLIE path (Paeth Stage 1 + YCoCg-R, as `pkon::encode_ellie` with the YCoCg-R representation): split planes ->
/// forward YCoCg-R -> one headerless Stage 4 stream per plane (Y 8-bit, Co/Cg 9-bit, separate EllieCodec per bit width) and the
/// reverse. Output = the concatenated plane streams only; w/h/layout and the three stream lengths are out-of-band (the lengths are
/// held in a side table keyed by the payload, as the PKON container keeps them in its metadata). Transform costs are timed.
struct Ellie;
static LENS: std::sync::Mutex<Vec<(u64, Vec<usize>)>> = std::sync::Mutex::new(Vec::new());
fn pay_key(b: &[u8]) -> u64 {
    let mut h = 0xcbf29ce484222325u64 ^ b.len() as u64;
    for &x in b.iter().take(64).chain(b.iter().rev().take(64)) {
        h = (h ^ x as u64).wrapping_mul(0x100000001b3);
    }
    h
}
fn bits_of(k: usize) -> u32 {
    if k == 1 || k == 2 { nanoscope::ellie::YCOCG_CHROMA_BITS } else { 8 }
}
impl Codec for Ellie {
    fn name(&self) -> String {
        "ellie".into()
    }
    fn version(&self) -> String {
        "ELLIE Paeth Stage 1 + YCoCg-R, per-plane Stage 4 streams".into()
    }
    fn config(&self) -> String {
        String::new()
    }
    fn supports(&self, spec: &Spec) -> bool {
        spec.bit_depth == 8 && spec.layout == Layout::Rgb
    }
    fn encode(&self, img: &Canonical) -> Result<Vec<u8>, CodecError> {
        let (w, h) = (img.spec.width as usize, img.spec.height as usize);
        let mut planes = split_planes(img);
        if !nanoscope::ellie::ycocg_r_forward_planes(&mut planes[..3]) {
            return Err(CodecError::Failed("ycocg range".into()));
        }
        let mut payload = Vec::new();
        let mut lens = vec![];
        let mut enc = Stage4Enc::with_capacity(w * h);
        let mut codecs: Vec<(u32, EllieCodec)> = vec![];
        for (k, p) in planes.iter().enumerate() {
            let bits = bits_of(k);
            let at = match codecs.iter().position(|c| c.0 == bits) {
                Some(i) => i,
                None => {
                    codecs.push((bits, EllieCodec::new(w, h, bits).map_err(|e| CodecError::Failed(format!("{e:?}")))?));
                    codecs.len() - 1
                }
            };
            enc.reset();
            codecs[at].1.encode_planes(&[p.as_slice()], &mut enc).map_err(|e| CodecError::Failed(format!("{e:?}")))?;
            let bytes = enc.finish();
            lens.push(bytes.len());
            payload.extend_from_slice(bytes);
        }
        let key = pay_key(&payload);
        let mut t = LENS.lock().unwrap();
        if !t.iter().any(|e| e.0 == key) {
            t.push((key, lens));
        }
        Ok(payload)
    }
    fn decode(&self, enc: &[u8], hint: &Spec) -> Result<Canonical, CodecError> {
        let (w, h, ch) = (hint.width as usize, hint.height as usize, hint.layout.channels());
        let lens = LENS.lock().unwrap().iter().find(|e| e.0 == pay_key(enc)).map(|e| e.1.clone()).ok_or_else(|| CodecError::Failed("unknown payload".into()))?;
        if lens.iter().sum::<usize>() != enc.len() {
            return Err(CodecError::Failed("payload length mismatch".into()));
        }
        let mut planes = vec![vec![0u16; w * h]; ch];
        let mut codecs: Vec<(u32, EllieCodec)> = vec![];
        let mut at = 0usize;
        for (k, &len) in lens.iter().enumerate() {
            let bits = bits_of(k);
            let i = match codecs.iter().position(|c| c.0 == bits) {
                Some(i) => i,
                None => {
                    codecs.push((bits, EllieCodec::new(w, h, bits).map_err(|e| CodecError::Failed(format!("{e:?}")))?));
                    codecs.len() - 1
                }
            };
            codecs[i].1.decode_planes(&enc[at..at + len], &mut [planes[k].as_mut_slice()]).map_err(|e| CodecError::Failed(format!("{e:?}")))?;
            at += len;
        }
        if !nanoscope::ellie::ycocg_r_inverse_planes(&mut planes[..3]) {
            return Err(CodecError::Failed("ycocg inverse domain".into()));
        }
        let mut data = vec![0u8; w * h * ch];
        for (k, p) in planes.iter().enumerate() {
            for (i, &v) in p.iter().enumerate() {
                data[i * ch + k] = v as u8;
            }
        }
        Canonical::new(*hint, data).map_err(CodecError::Failed)
    }
}

/// ELLIE-only side measurements (kept separate from the cross-codec table): ideal -log2(p) bytes of the Stage 3 stream,
/// model-only (no coder) time, and the carry_model=true variant size. Not part of any ranking.
struct CostIo {
    bits: f64,
}
impl BitIo for CostIo {
    #[inline(always)]
    fn code(&mut self, p1: u32, bit: u32) -> u32 {
        let p = p1 as f64 / 65536.0;
        self.bits -= if bit == 1 { p.log2() } else { (1.0 - p).log2() };
        bit
    }
}
struct NullIo;
impl BitIo for NullIo {
    #[inline(always)]
    fn code(&mut self, _p1: u32, bit: u32) -> u32 {
        bit
    }
}
fn ellie_side(img: &Canonical) -> serde_json::Value {
    let (w, h) = (img.spec.width as usize, img.spec.height as usize);
    let planes = split_planes(img);
    let refs: Vec<&[u16]> = planes.iter().map(|p| p.as_slice()).collect();
    let mut codec = EllieCodec::new(w, h, 8).unwrap();
    let mut cost = CostIo { bits: 0.0 };
    codec.encode_planes(&refs, &mut cost).unwrap();
    let mut model_s = f64::MAX;
    for _ in 0..2 {
        let t = Instant::now();
        codec.encode_planes(&refs, &mut NullIo).unwrap();
        model_s = model_s.min(t.elapsed().as_secs_f64());
    }
    codec.carry_model = true;
    let mut enc = Stage4Enc::with_capacity(img.data.len());
    codec.encode_planes(&refs, &mut enc).unwrap();
    let carry_bytes = enc.finish().len();
    serde_json::json!({"ideal_stage3_bytes": cost.bits / 8.0, "model_only_enc_ms": model_s * 1e3, "carry_on_bytes": carry_bytes})
}

// ---------------------------------------------------------------- dataset + features

struct Img {
    name: String,
    meta: serde_json::Value,
    full: (u32, u32),
    full_label: String,
    notes: Vec<String>,
    img: Canonical,
    feat: Feat,
}

#[derive(Clone, Default)]
struct Feat {
    med_mae: f64,
    med_zero_frac: f64,
    mean_g: f64,
    sat_frac: f64,
}

/// Cheap content descriptors on the G plane of the crop (MED/LOCO-I residual magnitude, share of exactly-predicted
/// pixels, mean level, clipped share). Descriptors only, used to group results; not part of any codec.
fn features(img: &Canonical) -> Feat {
    let (w, h, ch) = (img.spec.width as usize, img.spec.height as usize, img.spec.layout.channels());
    let g = |x: usize, y: usize| img.data[(y * w + x) * ch + if ch >= 3 { 1 } else { 0 }] as i32;
    let (mut sum, mut zero, mut n, mut tot, mut sat) = (0f64, 0u64, 0u64, 0f64, 0u64);
    for y in 0..h {
        for x in 0..w {
            let v = g(x, y);
            tot += v as f64;
            if v == 0 || v == 255 {
                sat += 1;
            }
            if x == 0 || y == 0 {
                continue;
            }
            let (a, b, c) = (g(x - 1, y), g(x, y - 1), g(x - 1, y - 1));
            let p = if c >= a.max(b) { a.min(b) } else if c <= a.min(b) { a.max(b) } else { a + b - c };
            let r = (v - p).abs();
            sum += r as f64;
            zero += (r == 0) as u64;
            n += 1;
        }
    }
    let px = (w * h) as f64;
    Feat { med_mae: sum / n as f64, med_zero_frac: zero as f64 / n as f64, mean_g: tot / px, sat_frac: sat as f64 / px }
}

fn load_images(only: Option<usize>) -> Vec<Img> {
    let data = PathBuf::from("data");
    let manifest: Vec<serde_json::Value> = serde_json::from_str(&std::fs::read_to_string(data.join("dataset50.json")).expect("data/dataset50.json (run benchmarks/pkon2/build_manifest50.py)")).unwrap();
    let mut v = vec![];
    for (i, m) in manifest.iter().enumerate() {
        if let Some(n) = only {
            if ![0, n, manifest.len() - 1].contains(&i) {
                continue;
            }
        }
        let full = Canonical::load(&data.join(m["file"].as_str().unwrap())).unwrap();
        let crop = center_crop(&full, CROP);
        let feat = features(&crop);
        v.push(Img { name: m["name"].as_str().unwrap().into(), meta: m.clone(), full: (full.spec.width, full.spec.height), full_label: full.spec.label(), notes: full.notes.clone(), img: crop, feat });
    }
    v
}

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

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1e3
}

fn tiny_image() -> Canonical {
    let (w, h) = (33u32, 17u32);
    let mut x = 12345u32;
    let data = (0..w * h * 3)
        .map(|i| {
            x = x.wrapping_mul(1664525).wrapping_add(1013904223);
            ((i / 3) as u8).wrapping_add((x >> 24) as u8 & 7)
        })
        .collect();
    Canonical::new(Spec { width: w, height: h, layout: Layout::Rgb, bit_depth: 8 }, data).unwrap()
}

fn sha_hex(b: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(b).iter().map(|x| format!("{x:02x}")).collect()
}

fn main() {
    let smoke = std::env::args().any(|a| a == "--smoke");
    let imgs = load_images(if smoke { Some(30) } else { None });
    let reps = if smoke { 1 } else { REPS };
    let ffs: Vec<Ff> = vec![Ff { kind: Kind::Jxl(7), threads: 1 }, Ff { kind: Kind::Jxl(9), threads: 1 }, Ff { kind: Kind::Webp(6), threads: 1 }];
    let png = PngCodec;
    let pk = Ellie;
    let mut codecs: Vec<&dyn Codec> = vec![&pk];
    codecs.extend(ffs.iter().map(|f| f as &dyn Codec));
    codecs.push(&png);
    let tiny = tiny_image();

    println!("== preflight ==");
    for i in &imgs {
        let s = &i.img.spec;
        println!("{:<14} full {}x{} {} -> crop {}x{} {} sha256 {} notes {:?}", i.name, i.full.0, i.full.1, i.full_label, s.width, s.height, s.label(), &i.img.content_hash()[..16], i.notes);
    }
    let mut calib: Vec<(String, f64, f64)> = vec![];
    for c in &codecs {
        let _ = round_trip(*c, &tiny, 1);
        let t = round_trip(*c, &tiny, 5).unwrap_or_else(|e| panic!("{}: {e}", c.name()));
        assert!(matches!(t.verdict, Verdict::Exact), "{} not exact on tiny image", c.name());
        truncation_check(*c, &tiny, &t.encoded).unwrap_or_else(|e| panic!("{}: {e}", c.name()));
        println!("{:<12} tiny exact, truncation rejected; floor enc {:.1} ms dec {:.1} ms", c.name(), ms(t.enc_time), ms(t.dec_time));
        calib.push((c.name(), ms(t.enc_time), ms(t.dec_time)));
    }

    let mut recs: Vec<serde_json::Value> = vec![];
    let mut all_exact = true;
    let (t0, total_jobs) = (Instant::now(), imgs.len());
    for (n, i) in imgs.iter().enumerate() {
        let (px, raw) = ((i.img.spec.width * i.img.spec.height) as usize, i.img.spec.byte_len());
        for c in &codecs {
            let rec = match round_trip(*c, &i.img, reps) {
                Ok(t) => {
                    let mut valid = matches!(t.verdict, Verdict::Exact);
                    let mut note = match &t.verdict {
                        Verdict::Exact => String::new(),
                        Verdict::Mismatch(m) => m.to_string(),
                    };
                    let mut side = serde_json::Value::Null;
                    if c.name() == "ellie" {
                        side = serde_json::Value::Null;
                        // determinism: a fresh encode must reproduce the same bytes
                        if c.encode(&i.img).map(|b| b != t.encoded).unwrap_or(true) {
                            valid = false;
                            note += " nondeterministic";
                        }
                    }
                    let ov = overhead(&c.name(), &t.encoded).or_else(|| c.container_overhead(&t.encoded));
                    eprintln!("[{}/{}] {:<14} {:<10} {:>8} B enc {:>7.0} ms dec {:>7.0} ms valid {}", n + 1, total_jobs, i.name, c.name(), t.encoded.len(), ms(t.enc_time), ms(t.dec_time), valid);
                    all_exact &= valid;
                    serde_json::json!({"image": i.name, "codec": c.name(), "total_bytes": t.encoded.len(), "payload_bytes": ov.map(|o| t.encoded.len() as u64 - o), "raw_bytes": raw, "pixels": px,
                        "enc_ms": ms(t.enc_time), "dec_ms": ms(t.dec_time), "byte_exact": matches!(t.verdict, Verdict::Exact), "ellie_side": side, "valid": valid, "note": note,
                        "file_sha256": sha_hex(&t.encoded)})
                }
                Err(e) => {
                    eprintln!("{} {}: {e}", i.name, c.name());
                    all_exact = false;
                    serde_json::json!({"image": i.name, "codec": c.name(), "total_bytes": null, "raw_bytes": raw, "pixels": px, "valid": false, "byte_exact": false, "note": e.to_string()})
                }
            };
            recs.push(rec);
        }
        let el = t0.elapsed().as_secs_f64();
        let eta = el / (n + 1) as f64 * (total_jobs - n - 1) as f64;
        eprintln!("== {}/{} images done, elapsed {:.0}s, ETA {:.0}s ==", n + 1, total_jobs, el, eta);
    }
    println!("all valid+exact: {all_exact}");
    if smoke {
        println!("smoke complete (nothing written)");
        return;
    }
    let dir = PathBuf::from("benchmarks/ellie-full-bench").join(format!("{}-ellie-paeth-ycocg-vs-bench50", utc_stamp()));
    std::fs::create_dir_all(&dir).unwrap();
    let images: Vec<_> = imgs
        .iter()
        .map(|i| {
            let mut m = i.meta.clone();
            let o = m.as_object_mut().unwrap();
            o.insert("decoded_full_format".into(), i.full_label.clone().into());
            o.insert("decoded_full_width".into(), i.full.0.into());
            o.insert("decoded_full_height".into(), i.full.1.into());
            o.insert("crop_width".into(), i.img.spec.width.into());
            o.insert("crop_height".into(), i.img.spec.height.into());
            o.insert("crop_format".into(), i.img.spec.label().into());
            o.insert("crop_sha256".into(), i.img.content_hash().into());
            o.insert("decoder_notes".into(), i.notes.clone().into());
            o.insert("med_mae_g".into(), i.feat.med_mae.into());
            o.insert("med_zero_frac_g".into(), i.feat.med_zero_frac.into());
            o.insert("mean_g".into(), i.feat.mean_g.into());
            o.insert("clipped_frac_g".into(), i.feat.sat_frac.into());
            m
        })
        .collect();
    std::fs::write(dir.join("results.json"), serde_json::to_string_pretty(&serde_json::json!({"reps_best_of": reps, "crop": CROP, "calibration_ms": calib, "all_valid": all_exact, "images": images, "records": recs})).unwrap()).unwrap();
    println!("wrote {}", dir.display());
}
