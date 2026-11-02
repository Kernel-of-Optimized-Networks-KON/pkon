//! Three-image benchmark of the integrated ULISA codec (codec 7) against production LISA (codec 4) and production ELLIE (codec 6).
//!
//! Real encoded files only: every number is the length of a complete `.pkon` file produced by the production entry points
//! (`pkon::encode_raw`, `pkon::encode_standard_with`, `pkon::encode_ulisa_report`) and every file is decoded and compared with the
//! source mosaic before anything is reported. Train+validation images only; runs sequentially so timings are not contended.
//!
//! ELLIE arm ("same supported input representation"): the mosaic goes through the same LISA frame ULISA uses (frozen L1 where it
//! activates, four parity planes) and the planes are stored as the four channels of a 16-bit RGBA-layout image, untransformed, in
//! the production ELLIE container. Its encode time is measured from the RAW mosaic (frame + packing + `encode_standard_with`),
//! so it is comparable with ULISA's end-to-end encode.
//!
//! Usage: bench_ulisa_codec7 <source_manifest.json> <out.json> <id,id,id>
use nanoscope::canon::{Canonical, Layout, Spec};
use nanoscope::lisa::{CfaPattern, ColorFilter, ImageMetadata, SampleLayout};
use nanoscope::lisa_frame::BayerFrame;
use nanoscope::pkon::{self, Decoded, EllieRepr};
use serde_json::{json, Value};
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

fn run(id: &str, m: &Value) -> Value {
    let (w, h) = (m["width"].as_u64().unwrap() as usize, m["height"].as_u64().unwrap() as usize);
    let bd = m["bit_depth"].as_u64().unwrap() as u8;
    let bytes = std::fs::read(format!("data/praw1/canon/{id}.u16")).unwrap();
    let s: Vec<u16> = bytes.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
    drop(bytes);
    let meta = meta_of(m, w, h);
    eprintln!("{id}: {w}x{h} {bd}-bit");

    // ---- LISA, codec 4 (production)
    let (lisa_file, lisa_enc) = timed(|| pkon::encode_raw(&s, w, h, bd, meta.clone()).unwrap());
    let (dec, lisa_dec) = timed(|| pkon::decode(&lisa_file).unwrap());
    let lisa_exact = matches!(&dec, Decoded::Raw(r) if r.samples == s);
    drop(dec);
    eprintln!("  lisa done: {} B exact={lisa_exact}", lisa_file.len());

    // ---- ELLIE, codec 6, on the same frame's parity planes (best of 2 for timing)
    let make_ellie = || {
        let frame = BayerFrame::build(&s, bd, meta.clone()).unwrap();
        let coded = frame.normalize(&s);
        let planes = frame.split(&coded);
        let (pw, ph) = frame.plane_dims()[0];
        assert!(frame.plane_dims().iter().all(|&d| d == (pw, ph)), "benchmark images have even dimensions");
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
    let ((ellie_file, ellie_bare), ellie_enc) = best(2, || {
        let t = Instant::now();
        let (_frame, img) = make_ellie();
        let (f, bare) = timed(|| pkon::encode_standard_with(&img, EllieRepr::Untransformed).unwrap());
        ((f, bare), t.elapsed().as_secs_f64())
    });
    let (dec, ellie_dec) = best(2, || timed(|| pkon::decode(&ellie_file).unwrap()));
    let ellie_exact = {
        let (frame, _) = make_ellie();
        match dec {
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
                frame.denormalize(&mut mosaic).is_ok() && mosaic == s
            }
            _ => false,
        }
    };
    eprintln!("  ellie done: {} B exact={ellie_exact}", ellie_file.len());

    // ---- ULISA, codec 7 (best of 2 for timing)
    let ((ulisa_file, info), ulisa_enc) = best(2, || {
        let t = Instant::now();
        let r = pkon::encode_ulisa_report(&s, w, h, bd, meta.clone()).unwrap();
        (r, t.elapsed().as_secs_f64())
    });
    let (dec, ulisa_dec) = best(2, || timed(|| pkon::decode(&ulisa_file).unwrap()));
    let ulisa_exact = matches!(&dec, Decoded::Raw(r) if r.samples == s && r.metadata == meta && r.bit_depth == bd);
    eprintln!("  ulisa done: {} B exact={ulisa_exact}", ulisa_file.len());
    let hdr = pkon::parse_header(&ulisa_file).unwrap();

    let pct = |a: usize, b: usize| 100.0 * (a as f64 / b as f64 - 1.0);
    json!({
        "id": id, "width": w, "height": h, "bit_depth": bd,
        "lisa_codec4": {"bytes": lisa_file.len(), "enc_s": lisa_enc, "dec_s": lisa_dec, "exact": lisa_exact},
        "ellie_codec6": {"bytes": ellie_file.len(), "enc_s": ellie_enc, "enc_bare_s": ellie_bare, "dec_s": ellie_dec, "exact": ellie_exact, "vs_lisa_pct": pct(ellie_file.len(), lisa_file.len())},
        "ulisa_codec7": {
            "bytes": ulisa_file.len(), "enc_s": ulisa_enc, "dec_s": ulisa_dec, "exact": ulisa_exact, "vs_lisa_pct": pct(ulisa_file.len(), lisa_file.len()),
            "codec_id": hdr.codec, "meta_version": hdr.meta_version,
            "l1_active": info.lattice.is_some(), "l1": info.lattice.map(|l| json!({"origin": l.origin, "g": l.g, "coding_bit_depth": l.coding_bit_depth})),
            "coding_depth": info.coding_depth,
            "flank_samples": info.flank.samples, "flank_nonzero": info.flank.nonzero,
            "enc_ratio_vs_ellie": ulisa_enc / ellie_enc, "dec_ratio_vs_ellie": ulisa_dec / ellie_dec,
        },
    })
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let manifest: Value = serde_json::from_slice(&std::fs::read(&a[1]).unwrap()).unwrap();
    let mut rows = vec![];
    for id in a[3].split(',') {
        let m = manifest.as_array().unwrap().iter().find(|m| m["id"] == id).unwrap_or_else(|| panic!("{id} not in manifest"));
        let r = run(id, m);
        let exact = ["lisa_codec4", "ellie_codec6", "ulisa_codec7"].iter().all(|k| r[k]["exact"] == true);
        if !exact {
            eprintln!("MISMATCH: {}", serde_json::to_string_pretty(&r).unwrap());
            std::process::exit(3);
        }
        rows.push(r);
    }
    std::fs::write(&a[2], serde_json::to_vec_pretty(&rows).unwrap()).unwrap();
}
