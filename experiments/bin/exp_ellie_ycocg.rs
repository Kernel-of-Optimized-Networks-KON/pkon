//! ELLIE YCoCg-R cross-channel decorrelation experiment (isolated; production path untouched).
//!
//! Input convention (same as the PKON2/ELLIE benchmarks): each file is decoded once with `Canonical::load_bytes` to
//! interleaved 8-bit samples and centre-cropped to at most 1024x1024; each channel is one plane. Losslessness means
//! pixel-identical reconstruction of that raster (JPEG inputs: the decoded raster, not the original JPEG bytes).
//!
//! usage: exp_ellie_stage4 [--out DIR] [--reps N] [FILE...]
//! default files: data/portrait_0.jpg data/screen_0.png data/night_0.jpg
use nanoscope::canon::{first_mismatch, Canonical, Spec};
use nanoscope::ellie::{EllieCodec, Stage4Enc};
use nanoscope::pkon::{self, EllieRepr};
#[allow(unused_imports)]
use nanoscope::rangecoder::BitIo;
use serde_json::json;
use std::fmt::Write as _;
use std::path::PathBuf;
use std::time::Instant;

const CROP: u32 = 1024;
/// PKON v2 container header (README: "24-byte header plus payload"); ELLIE payloads are headerless, so this is reported
/// separately and is not part of the Stage 4 byte count.
const PKON_HEADER: usize = 24;

/// Model-only sink (no coder): isolates Stage 1-3 time.
#[allow(dead_code)]
struct NullIo;
impl BitIo for NullIo {
    #[inline(always)]
    fn code(&mut self, _p1: u32, bit: u32) -> u32 {
        bit
    }
}

/// Ideal information of the Stage 3 probability stream, -log2 P(bit), in bits. Also counts decisions.
#[allow(dead_code)]
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

#[inline(always)]
fn fwd(r: i32, g: i32, b: i32) -> (i32, i32, i32) {
    let co = r - b;
    let t = b + (co >> 1);
    let cg = g - t;
    let y = t + (cg >> 1);
    (y, co, cg)
}
#[inline(always)]
fn inv(y: i32, co: i32, cg: i32) -> (i32, i32, i32) {
    let t = y - (cg >> 1);
    let g = cg + t;
    let b = t - (co >> 1);
    (co + b, g, b)
}

fn forward_planes(p: &[Vec<u16>]) -> [Vec<u16>; 3] {
    let n = p[0].len();
    let (mut y, mut co, mut cg) = (vec![0u16; n], vec![0u16; n], vec![0u16; n]);
    for i in 0..n {
        let (a, b, c) = fwd(p[0][i] as i32, p[1][i] as i32, p[2][i] as i32);
        y[i] = a as u16;
        co[i] = (b + 255) as u16;
        cg[i] = (c + 255) as u16;
    }
    [y, co, cg]
}

fn inverse_planes(t: &[Vec<u16>], out: &mut [Vec<u16>]) {
    for i in 0..t[0].len() {
        let (r, g, b) = inv(t[0][i] as i32, t[1][i] as i32 - 255, t[2][i] as i32 - 255);
        out[0][i] = r as u16;
        out[1][i] = g as u16;
        out[2][i] = b as u16;
    }
}

fn best(v: &mut f64, t: f64) {
    *v = v.min(t);
}

struct Res {
    bytes: usize,
    per_plane: Vec<usize>,
    enc_s: f64,
    dec_s: f64,
    xf_fwd_s: f64,
    xf_inv_s: f64,
    det: bool,
    exact: bool,
}

/// ycocg=false: accepted baseline, all planes through one codec and one stream.
/// ycocg=true: YCoCg-R on planes 0..3 (Co/Cg stored +255 in a 9-bit codec), extra planes unchanged, one stream per plane.
fn run(img: &Canonical, planes: &[Vec<u16>], ycocg: bool, reps: usize) -> Res {
    let (w, h) = (img.spec.width as usize, img.spec.height as usize);
    let nch = planes.len();
    let bits_of = |k: usize| if ycocg && (k == 1 || k == 2) { 9 } else { 8 };
    let (mut enc_s, mut dec_s, mut xf, mut xi) = (f64::MAX, f64::MAX, f64::MAX, f64::MAX);
    let mut streams: Vec<Vec<u8>> = vec![];
    let (mut det, mut exact) = (true, true);
    let mut dec_planes = vec![vec![0u16; w * h]; nch];
    let mut rebuilt = vec![];
    for rep in 0..reps {
        let t = Instant::now();
        let work: Vec<Vec<u16>> = if ycocg {
            let [y, co, cg] = forward_planes(planes);
            let mut v = vec![y, co, cg];
            v.extend(planes[3..].iter().cloned());
            v
        } else {
            planes.to_vec()
        };
        let tf = t.elapsed().as_secs_f64();
        if ycocg {
            best(&mut xf, tf);
        }
        let t = Instant::now();
        let mut cur: Vec<Vec<u8>> = vec![];
        if ycocg {
            for k in 0..nch {
                let mut c = EllieCodec::new(w, h, bits_of(k)).unwrap();
                let mut e = Stage4Enc::with_capacity(w * h);
                c.encode_planes(&[work[k].as_slice()], &mut e).unwrap();
                cur.push(e.finish().to_vec());
            }
        } else {
            let refs: Vec<&[u16]> = work.iter().map(|p| p.as_slice()).collect();
            let mut c = EllieCodec::new(w, h, 8).unwrap();
            let mut e = Stage4Enc::with_capacity(img.data.len());
            c.encode_planes(&refs, &mut e).unwrap();
            cur.push(e.finish().to_vec());
        }
        best(&mut enc_s, t.elapsed().as_secs_f64() + if ycocg { tf } else { 0.0 });
        if rep == 0 {
            streams = cur;
        } else {
            det &= cur == streams;
        }
        let t = Instant::now();
        let mut dec: Vec<Vec<u16>> = vec![vec![0u16; w * h]; nch];
        if ycocg {
            for k in 0..nch {
                let mut c = EllieCodec::new(w, h, bits_of(k)).unwrap();
                c.decode_planes(&streams[k], &mut [dec[k].as_mut_slice()]).unwrap();
            }
        } else {
            let mut c = EllieCodec::new(w, h, 8).unwrap();
            let mut outs: Vec<&mut [u16]> = dec.iter_mut().map(|p| p.as_mut_slice()).collect();
            c.decode_planes(&streams[0], &mut outs).unwrap();
        }
        let td = t.elapsed().as_secs_f64();
        if ycocg {
            let t = Instant::now();
            inverse_planes(&dec[..3], &mut dec_planes);
            for k in 3..nch {
                dec_planes[k].copy_from_slice(&dec[k]);
            }
            let ti = t.elapsed().as_secs_f64();
            best(&mut xi, ti);
            best(&mut dec_s, td + ti);
        } else {
            dec_planes = dec;
            best(&mut dec_s, td);
        }
        interleave(&dec_planes, img.spec, &mut rebuilt);
        exact &= rebuilt == img.data;
    }
    let mut check = Canonical::new(img.spec, rebuilt).unwrap();
    check.notes.clear();
    exact &= first_mismatch(img, &check).is_none();
    Res { bytes: streams.iter().map(|s| s.len()).sum(), per_plane: streams.iter().map(|s| s.len()).collect(), enc_s, dec_s, xf_fwd_s: xf, xf_inv_s: xi, det, exact }
}

fn main() {
    if cfg!(debug_assertions) {
        eprintln!("use --release");
        std::process::exit(2);
    }
    // Exhaustive 8-bit RGB domain check (2^24 triples) plus transformed ranges.
    let t = Instant::now();
    let (mut bad, mut ymin, mut ymax, mut cmin, mut cmax, mut gmin, mut gmax) = (0u64, i32::MAX, i32::MIN, i32::MAX, i32::MIN, i32::MAX, i32::MIN);
    for r in 0..256 {
        for g in 0..256 {
            for b in 0..256 {
                let (y, co, cg) = fwd(r, g, b);
                if inv(y, co, cg) != (r, g, b) {
                    bad += 1;
                }
                ymin = ymin.min(y);
                ymax = ymax.max(y);
                cmin = cmin.min(co);
                cmax = cmax.max(co);
                gmin = gmin.min(cg);
                gmax = gmax.max(cg);
            }
        }
    }
    eprintln!("exhaustive 2^24: {bad} failures; Y {ymin}..{ymax} Co {cmin}..{cmax} Cg {gmin}..{gmax} ({:.2}s)", t.elapsed().as_secs_f64());
    assert_eq!(bad, 0);
    assert!(ymin >= 0 && ymax <= 255 && cmin >= -255 && cmax <= 255 && gmin >= -255 && gmax <= 255);

    let mut out_root = PathBuf::from("benchmarks/ellie-ycocg-r");
    let mut reps = 3usize;
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
    assert!(reps >= 2);
    let dir = out_root.join(format!("{}-ycocg-r", utc_stamp()));
    std::fs::create_dir_all(&dir).unwrap();
    let (mut tb, mut ty, mut raw_t) = (0usize, 0usize, 0usize);
    let (mut eb, mut ey, mut db, mut dy, mut xf, mut xi) = (0f64, 0f64, 0f64, 0f64, 0f64, 0f64);
    let (mut ex, mut dt) = (true, true);
    let mut md = String::from("# ELLIE YCoCg-R experiment\n\nRelease, best of reps. Bytes = Stage 4 payload (headerless). YCoCg-R: Y 8-bit codec, Co/Cg 9-bit codec (stored +255), alpha unchanged; one Stage 4 stream per plane (Stage4Dec is private to decode_planes, so codecs of different bit depth cannot share one stream; costs a few termination bytes per extra stream).\n\n| image | baseline B | YCoCg-R B | delta B | delta % | ratio base | ratio ycocg | planes (ycocg) |\n|---|---|---|---|---|---|---|---|\n");
    let mut rows = vec![];
    let mut prod_md = String::from("
## Production container (codec 6)

| image | mode | file B | header+meta B | payload B | encode s | decode s | |
|---|---|---|---|---|---|---|---|
");
    let mut prod_tot = [0usize; 3];
    for path in &files {
        let full = Canonical::load_bytes(std::fs::read(path).unwrap()).unwrap();
        let img = crop(full);
        assert_eq!(img.spec.bit_depth, 8);
        let planes = deinterleave(&img);
        assert!(planes.len() >= 3);
        let name = path.file_stem().unwrap().to_string_lossy().to_string();
        let raw = img.data.len();
        let a = run(&img, &planes, false, reps);
        let b = run(&img, &planes, true, reps);
        ex &= a.exact && b.exact;
        dt &= a.det && b.det;
        tb += a.bytes;
        ty += b.bytes;
        raw_t += raw;
        eb += a.enc_s;
        ey += b.enc_s;
        db += a.dec_s;
        dy += b.dec_s;
        xf += b.xf_fwd_s;
        xi += b.xf_inv_s;
        // Production container path (codec 6): sizes include the 32-byte header and the ELLIE metadata block.
        for (label, mode) in [("untransformed", EllieRepr::Untransformed), ("ycocg-r", EllieRepr::YCoCgR), ("auto", EllieRepr::Auto)] {
            let (mut te, mut td) = (f64::MAX, f64::MAX);
            let mut file = vec![];
            for _ in 0..reps {
                let t = Instant::now();
                file = pkon::encode_standard_with(&img, mode).unwrap();
                te = te.min(t.elapsed().as_secs_f64());
                let t = Instant::now();
                let back = pkon::decode(&file).unwrap();
                td = td.min(t.elapsed().as_secs_f64());
                let pkon::Decoded::Standard(back) = back else { panic!() };
                assert!(back.data == img.data && back.spec == img.spec, "{name} {label} not exact");
            }
            let hdr = pkon::parse_header(&file).unwrap();
            let overhead = pkon::FIXED_HEADER_LEN + hdr.meta_len as usize;
            let _ = writeln!(prod_md, "| {name} | {label} | {} | {overhead} | {} | {:.3} | {:.3} | repr {} |", file.len(), file.len() - overhead, te, td, file[pkon::FIXED_HEADER_LEN + 1]);
            prod_tot[match label { "untransformed" => 0, "ycocg-r" => 1, _ => 2 }] += file.len();
        }
        let d = b.bytes as i64 - a.bytes as i64;
        let _ = writeln!(md, "| {name} | {} | {} | {d:+} | {:+.2}% | {:.3} | {:.3} | {:?} |", a.bytes, b.bytes, d as f64 / a.bytes as f64 * 100.0, raw as f64 / a.bytes as f64, raw as f64 / b.bytes as f64, b.per_plane);
        rows.push(json!({"image": name, "raw": raw, "baseline": a.bytes, "ycocg": b.bytes, "ycocg_planes": b.per_plane,
            "enc_s": [a.enc_s, b.enc_s], "dec_s": [a.dec_s, b.dec_s], "xf_fwd_s": b.xf_fwd_s, "xf_inv_s": b.xf_inv_s,
            "exact": [a.exact, b.exact], "deterministic": [a.det, b.det]}));
        eprintln!("{name}: base {} ycocg {} ({d:+}) exact={} det={}", a.bytes, b.bytes, a.exact && b.exact, a.det && b.det);
    }
    let d = ty as i64 - tb as i64;
    let _ = writeln!(md, "| **aggregate** | {tb} | {ty} | {d:+} | {:+.2}% | {:.3} | {:.3} | |", d as f64 / tb as f64 * 100.0, raw_t as f64 / tb as f64, raw_t as f64 / ty as f64);
    let _ = writeln!(md, "\nBaseline expected 3572101: {}.\n\nTiming (sum over images, best-of-reps each): encode base {eb:.3}s / ycocg {ey:.3}s; decode base {db:.3}s / ycocg {dy:.3}s; transform fwd {xf:.4}s, inverse {xi:.4}s (included in ycocg totals).\nRound trip all exact: {ex}. Deterministic: {dt}.", if tb == 3572101 { "REPRODUCED" } else { "NOT reproduced" });
    md.push_str(&prod_md);
    let _ = writeln!(md, "
Production file totals: untransformed {} B, ycocg-r {} B, auto {} B.", prod_tot[0], prod_tot[1], prod_tot[2]);
    std::fs::write(dir.join("report.md"), &md).unwrap();
    std::fs::write(dir.join("results.json"), serde_json::to_vec_pretty(&json!({"images": rows, "baseline_total": tb, "ycocg_total": ty})).unwrap()).unwrap();
    println!("{md}");
    if !(ex && dt) {
        std::process::exit(1);
    }
}
