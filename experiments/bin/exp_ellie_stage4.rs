//! ELLIE Stage 1-4 end-to-end experiment on real PNG/JPEG images.
//!
//! Input convention (same as the PKON2/ELLIE benchmarks): each file is decoded once with `Canonical::load_bytes` to
//! interleaved 8-bit samples and centre-cropped to at most 1024x1024; each channel is one plane. Losslessness means
//! pixel-identical reconstruction of that raster (JPEG inputs: the decoded raster, not the original JPEG bytes).
//!
//! usage: exp_ellie_stage4 [--out DIR] [--reps N] [FILE...]
//! default files: data/portrait_0.jpg data/screen_0.png data/night_0.jpg
use nanoscope::canon::{first_mismatch, Canonical, Spec};
use nanoscope::ellie::{EllieCodec, Stage4Enc};
use nanoscope::rangecoder::{BinEnc, BitIo};
use serde_json::{json, Value};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::Instant;

const CROP: u32 = 1024;
/// PKON v2 container header (README: "24-byte header plus payload"); ELLIE payloads are headerless, so this is reported
/// separately and is not part of the Stage 4 byte count.
const PKON_HEADER: usize = 24;

/// Model-only sink (no coder): isolates Stage 1-3 time.
struct NullIo;
impl BitIo for NullIo {
    #[inline(always)]
    fn code(&mut self, _p1: u32, bit: u32) -> u32 {
        bit
    }
}

/// Ideal information of the Stage 3 probability stream, -log2 P(bit), in bits. Also counts decisions.
struct CostIo {
    bits: f64,
    decisions: u64,
}
impl BitIo for CostIo {
    #[inline(always)]
    fn code(&mut self, p1: u32, bit: u32) -> u32 {
        let p = p1 as f64 / 65536.0;
        self.bits -= if bit == 1 { p.log2() } else { (1.0 - p).log2() };
        self.decisions += 1;
        bit
    }
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

/// Centre-crop to at most CROP x CROP (8-bit interleaved only; the corpus is 8-bit).
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
    let n = c.spec.width as usize * c.spec.height as usize;
    let mut planes = vec![vec![0u16; n]; ch];
    for (i, px) in c.data.chunks_exact(ch).enumerate() {
        for (k, &v) in px.iter().enumerate() {
            planes[k][i] = v as u16;
        }
    }
    planes
}

fn interleave(planes: &[Vec<u16>], spec: Spec, data: &mut Vec<u8>) {
    let ch = planes.len();
    data.clear();
    data.resize(planes[0].len() * ch, 0);
    for (k, p) in planes.iter().enumerate() {
        for (i, &v) in p.iter().enumerate() {
            data[i * ch + k] = v as u8;
        }
    }
    debug_assert_eq!(data.len(), spec.byte_len());
}

struct Run {
    carry: bool,
    bytes: usize,
    bytes_before_finish: usize,
    enc_s: f64,
    dec_s: f64,
    model_only_s: f64,
    setup_s: f64,
    ideal_bits: f64,
    decisions: u64,
    binenc_bytes: usize,
    deterministic: bool,
    exact: bool,
    stream: Vec<u8>,
}

fn best(v: &mut f64, t: f64) {
    *v = v.min(t);
}

fn measure(img: &Canonical, planes: &[Vec<u16>], carry: bool, reps: usize) -> Run {
    let (w, h) = (img.spec.width as usize, img.spec.height as usize);
    let refs: Vec<&[u16]> = planes.iter().map(|p| p.as_slice()).collect();

    let t = Instant::now();
    let mut codec = EllieCodec::new(w, h, 8).unwrap();
    codec.carry_model = carry;
    let mut enc = Stage4Enc::with_capacity(img.data.len());
    let mut out_planes = vec![vec![0u16; w * h]; planes.len()];
    let mut rebuilt = Vec::with_capacity(img.data.len());
    let setup_s = t.elapsed().as_secs_f64();

    let (mut enc_s, mut dec_s, mut model_only_s) = (f64::MAX, f64::MAX, f64::MAX);
    let (mut stream, mut before) = (Vec::new(), 0);
    let (mut deterministic, mut exact) = (true, true);
    for rep in 0..reps {
        enc.reset();
        let t = Instant::now();
        codec.encode_planes(&refs, &mut enc).unwrap();
        let pre = enc.bytes_so_far();
        let bytes = enc.finish();
        best(&mut enc_s, t.elapsed().as_secs_f64());
        if rep == 0 {
            stream = bytes.to_vec();
            before = pre;
        } else {
            deterministic &= bytes == &stream[..];
        }

        let t = Instant::now();
        {
            let mut outs: Vec<&mut [u16]> = out_planes.iter_mut().map(|p| p.as_mut_slice()).collect();
            codec.decode_planes(&stream, &mut outs).unwrap();
        }
        best(&mut dec_s, t.elapsed().as_secs_f64());
        interleave(&out_planes, img.spec, &mut rebuilt);
        exact &= rebuilt == img.data;

        let t = Instant::now();
        codec.encode_planes(&refs, &mut NullIo).unwrap();
        best(&mut model_only_s, t.elapsed().as_secs_f64());
    }
    // An independently constructed codec must reproduce the same bytes.
    let mut fresh = EllieCodec::new(w, h, 8).unwrap();
    fresh.carry_model = carry;
    let mut e2 = Stage4Enc::new();
    fresh.encode_planes(&refs, &mut e2).unwrap();
    deterministic &= e2.finish() == &stream[..];

    let mut cost = CostIo { bits: 0.0, decisions: 0 };
    codec.encode_planes(&refs, &mut cost).unwrap();
    let mut be = BinEnc::new();
    codec.encode_planes(&refs, &mut be).unwrap();
    let binenc_bytes = be.finish().len();

    // Cross-check against the decoded raster with the repo's own comparator.
    let mut check = Canonical::new(img.spec, rebuilt).unwrap();
    check.notes.clear();
    exact &= first_mismatch(img, &check).is_none();

    Run {
        carry,
        bytes: stream.len(),
        bytes_before_finish: before,
        enc_s,
        dec_s,
        model_only_s,
        setup_s,
        ideal_bits: cost.bits,
        decisions: cost.decisions,
        binenc_bytes,
        deterministic,
        exact,
        stream,
    }
}

fn main() {
    let mut out_root = PathBuf::from("benchmarks/ellie-stage4-3img");
    let mut reps = 3usize;
    let mut files: Vec<PathBuf> = vec![];
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--out" => out_root = PathBuf::from(args.next().expect("--out DIR")),
            "--reps" => reps = args.next().expect("--reps N").parse().unwrap(),
            _ => files.push(PathBuf::from(a)),
        }
    }
    if files.is_empty() {
        files = ["data/portrait_0.jpg", "data/screen_0.png", "data/night_0.jpg"].map(PathBuf::from).to_vec();
    }
    assert!(reps >= 2, "need at least 2 reps for the determinism check");
    if cfg!(debug_assertions) {
        eprintln!("refusing to measure a debug build; use --release");
        std::process::exit(2);
    }
    let dir = out_root.join(format!("{}-stage4-three-image", utc_stamp()));
    std::fs::create_dir_all(dir.join("streams")).unwrap();

    // Reader thread: the main thread blocks in recv() (parked by the OS) while files load, instead of spinning.
    let (tx, rx) = mpsc::sync_channel::<(PathBuf, std::io::Result<Vec<u8>>)>(1);
    let paths = files.clone();
    let reader = std::thread::spawn(move || {
        for p in paths {
            let r = std::fs::read(&p);
            if tx.send((p, r)).is_err() {
                break;
            }
        }
    });

    let mut rows: Vec<Value> = vec![];
    let mut report = String::new();
    let mut tot = [0u64; 8]; // file, canonical, bytes(carry=false), bytes(carry=true), ideal bits(false)*, enc ns, dec ns, binenc
    let mut agg_ideal = 0f64;
    let (mut agg_enc, mut agg_dec) = (0f64, 0f64);
    let mut all_exact = true;
    let mut all_det = true;
    for _ in 0..files.len() {
        let (path, bytes) = rx.recv().expect("reader thread ended early");
        let bytes = bytes.unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        let file_bytes = bytes.len() as u64;
        let format = if bytes.starts_with(&[0x89, b'P', b'N', b'G']) { "png" } else if bytes.starts_with(&[0xFF, 0xD8]) { "jpeg" } else { "other" };
        let t = Instant::now();
        let full = Canonical::load_bytes(bytes).unwrap();
        let (src_w, src_h) = (full.spec.width, full.spec.height);
        let img = crop(full);
        let load_s = t.elapsed().as_secs_f64();
        assert_eq!(img.spec.bit_depth, 8);
        let planes = deinterleave(&img);
        let name = path.file_stem().unwrap().to_string_lossy().to_string();
        let raw = img.data.len() as u64;
        let n_samples = raw as f64;

        let runs: Vec<Run> = [false, true].iter().map(|&c| measure(&img, &planes, c, reps)).collect();
        for r in &runs {
            all_exact &= r.exact;
            all_det &= r.deterministic;
            std::fs::write(dir.join("streams").join(format!("{name}.carry{}.ellie4", r.carry as u8)), &r.stream).unwrap();
        }
        let a = &runs[0];
        tot[0] += file_bytes;
        tot[1] += raw;
        tot[2] += runs[0].bytes as u64;
        tot[3] += runs[1].bytes as u64;
        tot[7] += runs[0].binenc_bytes as u64;
        agg_ideal += a.ideal_bits;
        agg_enc += a.enc_s;
        agg_dec += a.dec_s;

        let mut jr = vec![];
        for r in &runs {
            let ideal_bytes = r.ideal_bits / 8.0;
            jr.push(json!({
                "stage3_carry_model": r.carry,
                "encoded_bytes": r.bytes,
                "ratio_vs_canonical": raw as f64 / r.bytes as f64,
                "bits_per_sample": r.bytes as f64 * 8.0 / n_samples,
                "ideal_stage3_bytes": ideal_bytes,
                "stage4_overhead_bytes": r.bytes as f64 - ideal_bytes,
                "stage4_overhead_pct": (r.bytes as f64 / ideal_bytes - 1.0) * 100.0,
                "shared_binenc_5byte_flush_bytes": r.binenc_bytes,
                "termination_saving_vs_5byte_flush": r.binenc_bytes as i64 - r.bytes as i64,
                "bytes_committed_before_finish": r.bytes_before_finish,
                "binary_decisions": r.decisions,
                "encode_s_best": r.enc_s,
                "decode_s_best": r.dec_s,
                "model_only_encode_s_best": r.model_only_s,
                "setup_s": r.setup_s,
                "encode_MBps": raw as f64 / r.enc_s / 1e6,
                "decode_MBps": raw as f64 / r.dec_s / 1e6,
                "stage4_encode_share_pct": (1.0 - r.model_only_s / r.enc_s) * 100.0,
                "deterministic": r.deterministic,
                "exact": r.exact,
            }));
        }
        rows.push(json!({
            "image": name, "path": path.to_string_lossy(), "format": format, "source_px": [src_w, src_h],
            "crop_px": [img.spec.width, img.spec.height], "layout": img.spec.layout.name(), "channels": planes.len(),
            "file_bytes": file_bytes, "canonical_bytes": raw, "content_sha256": img.content_hash(),
            "load_decode_crop_s": load_s, "runs": jr,
        }));
        let _ = writeln!(
            report,
            "| {name} | {format} | {src_w}x{src_h} | {}x{} {} | {file_bytes} | {raw} | {} | {:.3} | {} | {:.3} | {:.2} | {:.2} | {:.2} | {} / {} |",
            img.spec.width, img.spec.height, img.spec.layout.name(),
            runs[0].bytes, raw as f64 / runs[0].bytes as f64,
            runs[1].bytes, raw as f64 / runs[1].bytes as f64,
            raw as f64 / runs[0].enc_s / 1e6, raw as f64 / runs[0].dec_s / 1e6,
            (runs[0].bytes as f64 / (runs[0].ideal_bits / 8.0) - 1.0) * 100.0,
            if runs[0].exact { "exact" } else { "MISMATCH" }, if runs[0].deterministic { "det" } else { "NONDET" },
        );
        eprintln!("{name}: {} B (carry off) / {} B (carry on), exact={} det={}", runs[0].bytes, runs[1].bytes, runs[0].exact && runs[1].exact, runs[0].deterministic && runs[1].deterministic);
    }
    reader.join().unwrap();

    let headline_carry = tot[3] < tot[2];
    let (hb, label) = if headline_carry { (tot[3], "carry on") } else { (tot[2], "carry off") };
    let agg = json!({
        "images": files.len(),
        "file_bytes": tot[0], "canonical_bytes": tot[1],
        "encoded_bytes_carry_off": tot[2], "encoded_bytes_carry_on": tot[3],
        "ratio_carry_off": tot[1] as f64 / tot[2] as f64, "ratio_carry_on": tot[1] as f64 / tot[3] as f64,
        "ideal_stage3_bytes_carry_off": agg_ideal / 8.0,
        "stage4_overhead_pct_carry_off": (tot[2] as f64 / (agg_ideal / 8.0) - 1.0) * 100.0,
        "shared_binenc_bytes_carry_off": tot[7],
        "encode_MBps_carry_off": tot[1] as f64 / agg_enc / 1e6,
        "decode_MBps_carry_off": tot[1] as f64 / agg_dec / 1e6,
        "all_exact": all_exact, "all_deterministic": all_det,
        "headline_mode": label, "headline_bytes": hb,
    });
    let meta = json!({
        "reps": reps, "crop": CROP, "build": "release", "timing": "best of reps; encode = Stage 1-4 + finish, decode = Stage 4 + inverse 3-1; setup (model allocation) and plane extraction timed separately/excluded",
        "pkon_header_bytes_not_included": PKON_HEADER,
    });
    std::fs::write(dir.join("results.json"), serde_json::to_vec_pretty(&json!({"meta": meta, "images": rows, "aggregate": agg})).unwrap()).unwrap();

    let mut md = String::new();
    let _ = writeln!(md, "# ELLIE Stage 1-4 three-image experiment\n");
    let _ = writeln!(md, "Release build, best of {reps} reps. Raster = decoded 8-bit samples, centre-cropped to at most {CROP}x{CROP}; per-channel planes, Stage 2/3 restart per plane (carry off) unless noted. Ratio = canonical raw bytes / encoded payload bytes (headerless; a PKON v2 container would add {PKON_HEADER} B). Throughput is MB/s of canonical raw bytes. Overhead = Stage 4 bytes vs the ideal -log2(p) information of the Stage 3 probability stream.\n");
    let _ = writeln!(md, "| image | fmt | source | raster | file B | canonical B | ELLIE B (carry off) | ratio | ELLIE B (carry on) | ratio | enc MB/s | dec MB/s | S4 overhead % | round trip |");
    let _ = writeln!(md, "|---|---|---|---|---|---|---|---|---|---|---|---|---|---|");
    md.push_str(&report);
    let _ = writeln!(
        md,
        "| **aggregate** | | | | {} | {} | {} | {:.3} | {} | {:.3} | {:.2} | {:.2} | {:.4} | {} |",
        tot[0], tot[1], tot[2], tot[1] as f64 / tot[2] as f64, tot[3], tot[1] as f64 / tot[3] as f64,
        tot[1] as f64 / agg_enc / 1e6, tot[1] as f64 / agg_dec / 1e6, (tot[2] as f64 / (agg_ideal / 8.0) - 1.0) * 100.0,
        if all_exact { "all exact" } else { "MISMATCH" },
    );
    let _ = writeln!(md, "\nAggregate ratios are from summed bytes. Shared `BinEnc` (5-byte flush) on the same stream totals {} B vs Stage 4's {} B (carry off). Determinism across repeated and fresh-codec encodes: {}.", tot[7], tot[2], if all_det { "identical bytes" } else { "FAILED" });
    let _ = writeln!(md, "\nPer-image detail (overhead bytes, termination, model-only vs full timing) is in `results.json`.");
    std::fs::write(dir.join("report.md"), md).unwrap();
    println!("{}", Path::new(&dir).display());
    if !(all_exact && all_det) {
        std::process::exit(1);
    }
}
