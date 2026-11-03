//! Full-corpus benchmark of the frozen production codecs LISA (4), ELLIE (6), ULISA (7) against lossless JPEG XL (effort 7, 9) and PNG
//! on the frozen 25-image u16 RAW corpus (`data/praw1/canon`, manifest `benchmarks/lisa-vs-jxl-png/source_manifest.json`).
//!
//! Benchmarking only: no codec code is touched. Every number is the length of a real file written to `<out>/files/` and every file is
//! decoded from its own bytes and compared sample-by-sample with the source before it is accepted.
//!  - LISA/ULISA: production entry points (`pkon::encode_raw`, `pkon::encode_ulisa_report`) / `pkon::decode`.
//!  - ELLIE arm: identical to `bench_ulisa_codec7` (LISA frame parity planes -> 16-bit RGBA -> `encode_standard_with(Untransformed)`).
//!  - JXL: ffmpeg libjxl `-distance 0 -threads 1`, gray16le in, bare codestream out (ff 0a checked), single-threaded.
//!  - PNG: Rust `png` through `image` (`nanoscope::codec::PngCodec`, Best + Adaptive), Gray 16-bit.
//! Timing: best of 2 per image/codec/direction, sequential. ffmpeg times include process spawn + temp-file I/O (floor calibrated on 16x16).
//!
//! Usage: bench_ulisa_codec7_corpus <source_manifest.json> <out_dir> [id,id,...]
use nanoscope::canon::{Canonical, Layout, Spec};
use nanoscope::codec::{Codec, PngCodec};
use nanoscope::lisa::{CfaPattern, ColorFilter, ImageMetadata, SampleLayout};
use nanoscope::lisa_frame::BayerFrame;
use nanoscope::pkon::{self, Decoded, EllieRepr};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Instant;

fn meta_of(m: &Value, w: usize, h: usize) -> ImageMetadata {
    let dim: Vec<usize> = m["cfa_dim"].as_array().unwrap().iter().map(|v| v.as_u64().unwrap() as usize).collect();
    let org: Vec<usize> = m["cfa_origin"].as_array().unwrap().iter().map(|v| v.as_u64().unwrap() as usize).collect();
    let entries: Vec<ColorFilter> = m["cfa"].as_array().unwrap().iter().map(|v| ColorFilter::try_from(v.as_u64().unwrap() as u8).unwrap()).collect();
    ImageMetadata::with_cfa(w, h, SampleLayout::BayerCfa, CfaPattern { width: dim[1], height: dim[0], entries, origin_x: org[0], origin_y: org[1] })
}

fn timed<T>(f: impl FnOnce() -> T) -> (T, f64) {
    let t = Instant::now();
    let r = f();
    (r, t.elapsed().as_secs_f64())
}

fn best<T>(reps: usize, mut f: impl FnMut() -> (T, f64)) -> (T, f64) {
    let (mut keep, mut bt) = f();
    for _ in 1..reps {
        let (r, t) = f();
        if t < bt {
            keep = r;
            bt = t;
        }
    }
    (keep, bt)
}

fn sha_hex(b: &[u8]) -> String {
    Sha256::digest(b).iter().map(|x| format!("{x:02x}")).collect()
}

fn ffmpeg(args: &[String]) -> Result<(), String> {
    let out = Command::new("ffmpeg")
        .args(["-hide_banner", "-loglevel", "error", "-xerror", "-y", "-nostdin"])
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .map_err(|e| e.to_string())?;
    if out.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).lines().last().unwrap_or("ffmpeg failed").to_string())
    }
}

fn s(x: &str) -> String {
    x.to_string()
}

/// JXL lossless encode of a gray16le raw file to `outp` (bare codestream).
fn jxl_encode(raw: &Path, w: usize, h: usize, effort: u8, outp: &Path) -> Result<(), String> {
    ffmpeg(&[
        s("-f"), s("rawvideo"), s("-pix_fmt"), s("gray16le"), s("-video_size"), format!("{w}x{h}"), s("-i"), raw.to_string_lossy().into(),
        s("-frames:v"), s("1"), s("-c:v"), s("libjxl"), s("-distance"), s("0"), s("-effort"), effort.to_string(), s("-pix_fmt"), s("gray16le"),
        s("-threads"), s("1"), s("-update"), s("1"), s("-f"), s("image2"), outp.to_string_lossy().into(),
    ])
}

fn jxl_decode(jxl: &Path, outraw: &Path) -> Result<(), String> {
    ffmpeg(&[
        s("-threads"), s("1"), s("-i"), jxl.to_string_lossy().into(), s("-frames:v"), s("1"), s("-f"), s("rawvideo"), s("-pix_fmt"), s("gray16le"),
        outraw.to_string_lossy().into(),
    ])
}

/// ffmpeg wraps 16-bit gray libjxl output in an ISOBMFF container (the RGB8 bench got a bare stream). Return the bare codestream
/// (concatenated `jxlc` / `jxlp` payloads) and the number of container bytes removed. Bare input passes through unchanged.
fn bare_codestream(f: &[u8]) -> Result<(Vec<u8>, usize), String> {
    if f.starts_with(&[0xff, 0x0a]) {
        return Ok((f.to_vec(), 0));
    }
    let (mut pos, mut out) = (0usize, vec![]);
    while pos + 8 <= f.len() {
        let size = u32::from_be_bytes(f[pos..pos + 4].try_into().unwrap()) as usize;
        let ty = &f[pos + 4..pos + 8];
        let (hdr, size) = if size == 1 { (16, u64::from_be_bytes(f[pos + 8..pos + 16].try_into().unwrap()) as usize) } else if size == 0 { (8, f.len() - pos) } else { (8, size) };
        if size < hdr || pos + size > f.len() {
            return Err("bad JXL box".into());
        }
        match ty {
            b"jxlc" => out.extend_from_slice(&f[pos + hdr..pos + size]),
            b"jxlp" => out.extend_from_slice(&f[pos + hdr + 4..pos + size]),
            _ => {}
        }
        pos += size;
    }
    if out.starts_with(&[0xff, 0x0a]) {
        Ok((out.clone(), f.len() - out.len()))
    } else {
        Err("no bare codestream found in JXL container".into())
    }
}

fn show(r: &Result<Value, String>) -> String {
    match r {
        Ok(v) => format!("{} B exact={}", v["bytes"], v["exact"]),
        Err(e) => e.clone(),
    }
}

fn run(id: &str, m: &Value, out: &Path, tmp: &Path) -> Value {
    let (w, h) = (m["width"].as_u64().unwrap() as usize, m["height"].as_u64().unwrap() as usize);
    let bd = m["bit_depth"].as_u64().unwrap() as u8;
    let bytes = std::fs::read(format!("data/praw1/canon/{id}.u16")).unwrap();
    let src_sha = sha_hex(&bytes);
    let sha_ok = src_sha == m["canon_sha256"].as_str().unwrap() && bytes.len() == w * h * 2;
    let s16: Vec<u16> = bytes.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
    let meta = meta_of(m, w, h);
    let files = out.join("files");
    eprintln!("{id}: {w}x{h} {bd}-bit source_sha_ok={sha_ok}");
    let mut res = json!({"id": id, "width": w, "height": h, "bit_depth": bd, "pixels": w * h, "raw_bytes": w * h * 2, "source_sha256": src_sha, "source_sha_ok": sha_ok});

    // ---- LISA, codec 4
    let r = (|| -> Result<Value, String> {
        let (f, enc) = best(2, || {
            let t = Instant::now();
            let f = pkon::encode_raw(&s16, w, h, bd, meta.clone()).map_err(|e| format!("{e:?}"));
            (f, t.elapsed().as_secs_f64())
        });
        let f = f?;
        std::fs::write(files.join(format!("{id}.lisa4.pkon")), &f).unwrap();
        let (d, dec) = best(2, || timed(|| pkon::decode(&f).map_err(|e| format!("{e:?}"))));
        let exact = matches!(d?, Decoded::Raw(r) if r.samples == s16 && r.metadata == meta && r.bit_depth == bd);
        Ok(json!({"bytes": f.len(), "enc_s": enc, "dec_s": dec, "exact": exact, "sha256": sha_hex(&f)}))
    })();
    eprintln!("  lisa4   {}", show(&r));
    res["lisa4"] = r.unwrap_or_else(|e| json!({"error": e}));

    // ---- ELLIE (codec 6, on the LISA frame parity planes) and ULISA (codec 7)
    let frame_dims = BayerFrame::build(&s16, bd, meta.clone()).ok().map(|f| f.plane_dims());
    if let Some(dims) = frame_dims {
        let ellie_packable = dims.iter().all(|&d| d == dims[0]);
        let make_ellie = || {
            let frame = BayerFrame::build(&s16, bd, meta.clone()).unwrap();
            let coded = frame.normalize(&s16);
            let planes = frame.split(&coded);
            let (pw, ph) = frame.plane_dims()[0];
            assert!(frame.plane_dims().iter().all(|&d| d == (pw, ph)), "even dimensions required");
            let mut data = vec![0u8; pw * ph * 4 * 2];
            for (k, p) in planes.iter().enumerate() {
                for (i, &v) in p.iter().enumerate() {
                    let o = (i * 4 + k) * 2;
                    data[o..o + 2].copy_from_slice(&v.to_le_bytes());
                }
            }
            let img = Canonical::new(Spec { width: pw as u32, height: ph as u32, layout: Layout::Rgba, bit_depth: 16 }, data).unwrap();
            (frame, img)
        };
        let r = (|| -> Result<Value, String> {
            if !ellie_packable {
                return Err("N/A: unequal parity-plane dimensions cannot be packed as one RGBA image (harness limit)".into());
            }
            let ((f, _bare), enc) = best(2, || {
                let t = Instant::now();
                let (_fr, img) = make_ellie();
                let (f, bare) = timed(|| pkon::encode_standard_with(&img, EllieRepr::Untransformed).map_err(|e| format!("{e:?}")));
                ((f, bare), t.elapsed().as_secs_f64())
            });
            let f = f?;
            std::fs::write(files.join(format!("{id}.ellie6.pkon")), &f).unwrap();
            let (d, dec) = best(2, || timed(|| pkon::decode(&f).map_err(|e| format!("{e:?}"))));
            let exact = {
                let (frame, _) = make_ellie();
                match d? {
                    Decoded::Standard(c) => {
                        let (pw, ph) = frame.plane_dims()[0];
                        let mut planes: [Vec<u16>; 4] = std::array::from_fn(|_| vec![0u16; pw * ph]);
                        for (k, p) in planes.iter_mut().enumerate() {
                            for (i, v) in p.iter_mut().enumerate() {
                                let o = (i * 4 + k) * 2;
                                *v = u16::from_le_bytes([c.data[o], c.data[o + 1]]);
                            }
                        }
                        let mut mosaic = frame.merge(&planes);
                        frame.denormalize(&mut mosaic).is_ok() && mosaic == s16
                    }
                    _ => false,
                }
            };
            Ok(json!({"bytes": f.len(), "enc_s": enc, "dec_s": dec, "exact": exact, "sha256": sha_hex(&f)}))
        })();
        eprintln!("  ellie6  {}", show(&r));
        res["ellie6"] = r.unwrap_or_else(|e| json!({"error": e}));

        let r = (|| -> Result<Value, String> {
            let (r, enc) = best(2, || {
                let t = Instant::now();
                let r = pkon::encode_ulisa_report(&s16, w, h, bd, meta.clone()).map_err(|e| format!("{e:?}"));
                (r, t.elapsed().as_secs_f64())
            });
            let (f, info) = r?;
            std::fs::write(files.join(format!("{id}.ulisa7.pkon")), &f).unwrap();
            let (d, dec) = best(2, || timed(|| pkon::decode(&f).map_err(|e| format!("{e:?}"))));
            let exact = matches!(d?, Decoded::Raw(r) if r.samples == s16 && r.metadata == meta && r.bit_depth == bd);
            let hdr = pkon::parse_header(&f).map_err(|e| format!("{e:?}"))?;
            Ok(json!({"bytes": f.len(), "enc_s": enc, "dec_s": dec, "exact": exact, "sha256": sha_hex(&f), "codec_id": hdr.codec, "meta_version": hdr.meta_version,
                "l1_active": info.lattice.is_some(), "coding_depth": info.coding_depth, "flank_samples": info.flank.samples, "flank_nonzero": info.flank.nonzero}))
        })();
        eprintln!("  ulisa7  {}", show(&r));
        res["ulisa7"] = r.unwrap_or_else(|e| json!({"error": e}));
    } else {
        for k in ["ellie6", "ulisa7"] {
            res[k] = json!({"error": "not a supported 2x2 Bayer frame (BayerFrame::build failed)", "na": true});
        }
        eprintln!("  ellie6/ulisa7 N/A (BayerFrame::build failed)");
    }

    // ---- JXL e7 / e9 via ffmpeg (single-threaded)
    let rawp = tmp.join("in.raw");
    std::fs::write(&rawp, &bytes).unwrap();
    for effort in [7u8, 9] {
        let key = format!("jxl_e{effort}");
        let r = (|| -> Result<Value, String> {
            let jp = files.join(format!("{id}.jxl-e{effort}.jxl"));
            let mut enc_best = f64::MAX;
            for _ in 0..2 {
                let t = Instant::now();
                jxl_encode(&rawp, w, h, effort, &jp)?;
                enc_best = enc_best.min(t.elapsed().as_secs_f64());
            }
            let (f, container_removed) = bare_codestream(&std::fs::read(&jp).map_err(|e| e.to_string())?)?;
            std::fs::write(&jp, &f).map_err(|e| e.to_string())?;
            let outraw = tmp.join("dec.raw");
            let mut dec_best = f64::MAX;
            for _ in 0..2 {
                let t = Instant::now();
                jxl_decode(&jp, &outraw)?;
                dec_best = dec_best.min(t.elapsed().as_secs_f64());
            }
            let exact = std::fs::read(&outraw).map_err(|e| e.to_string())? == bytes;
            let _ = std::fs::remove_file(&outraw);
            Ok(json!({"bytes": f.len(), "enc_s": enc_best, "dec_s": dec_best, "exact": exact, "sha256": sha_hex(&f), "container_bytes_removed": container_removed}))
        })();
        eprintln!("  {key:<7} {}", show(&r));
        res[key.as_str()] = r.unwrap_or_else(|e| json!({"error": e}));
    }
    let _ = std::fs::remove_file(&rawp);

    // ---- PNG (Rust png via image), 16-bit gray
    let r = (|| -> Result<Value, String> {
        let img = Canonical::new(Spec { width: w as u32, height: h as u32, layout: Layout::Gray, bit_depth: 16 }, bytes.clone())?;
        let (f, enc) = best(2, || {
            let t = Instant::now();
            let f = PngCodec.encode(&img).map_err(|e| e.to_string());
            (f, t.elapsed().as_secs_f64())
        });
        let f = f?;
        std::fs::write(files.join(format!("{id}.png")), &f).unwrap();
        let (d, dec) = best(2, || timed(|| PngCodec.decode(&f, &img.spec).map_err(|e| e.to_string())));
        let d = d?;
        let exact = d.spec == img.spec && d.data == img.data;
        Ok(json!({"bytes": f.len(), "enc_s": enc, "dec_s": dec, "exact": exact, "sha256": sha_hex(&f)}))
    })();
    eprintln!("  png     {}", show(&r));
    res["png"] = r.unwrap_or_else(|e| json!({"error": e}));
    res
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let manifest: Value = serde_json::from_slice(&std::fs::read(&a[1]).unwrap()).unwrap();
    let out = PathBuf::from(&a[2]);
    std::fs::create_dir_all(out.join("files")).unwrap();
    let tmp = out.join("tmp");
    std::fs::create_dir_all(&tmp).unwrap();
    let ids: Vec<String> = match a.get(3) {
        Some(l) => l.split(',').map(String::from).collect(),
        None => manifest.as_array().unwrap().iter().map(|m| m["id"].as_str().unwrap().to_string()).collect(),
    };
    // ffmpeg per-call process floor (16x16, median of 5)
    let tiny = tmp.join("tiny.raw");
    std::fs::write(&tiny, vec![0u8; 16 * 16 * 2]).unwrap();
    let (tj, tr) = (tmp.join("t.jxl"), tmp.join("t.out"));
    let (mut fe, mut fd) = (vec![], vec![]);
    for _ in 0..5 {
        fe.push(timed(|| jxl_encode(&tiny, 16, 16, 7, &tj).unwrap()).1);
        fd.push(timed(|| jxl_decode(&tj, &tr).unwrap()).1);
    }
    fe.sort_by(|a, b| a.partial_cmp(b).unwrap());
    fd.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let floor = json!({"enc_s": fe[2], "dec_s": fd[2]});
    eprintln!("ffmpeg floor: {floor}");
    let mut f = std::fs::OpenOptions::new().create(true).append(true).open(out.join("results.jsonl")).unwrap();
    writeln!(f, "{}", json!({"floor": floor})).unwrap();
    for id in &ids {
        let m = manifest.as_array().unwrap().iter().find(|m| m["id"] == id.as_str()).unwrap_or_else(|| panic!("{id} not in manifest"));
        let r = run(id, m, &out, &tmp);
        writeln!(f, "{r}").unwrap();
        f.flush().unwrap();
    }
}
