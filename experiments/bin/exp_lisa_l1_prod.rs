//! L1 production promotion check: encode every corpus image through `pkon::encode_raw`, decode, verify exact reconstruction,
//! and record size, SHA-256 of the stream and timings. Run once on the pre-L1 tree and once on the L1 tree and compare.
//! Usage: exp_lisa_l1_prod <dataset_manifest.json> <out.json> [threads=4] [id,id,...]
use nanoscope::lisa::*;
use nanoscope::pkon::{self, Decoded};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::sync::Mutex;
use std::time::Instant;

fn meta_of(m: &Value, w: usize, h: usize) -> ImageMetadata {
    let Some(cfa) = m["cfa"].as_array().filter(|a| !a.is_empty()) else {
        return ImageMetadata::without_cfa(w, h, SampleLayout::Monochrome);
    };
    let dim: Vec<usize> = m["cfa_dim"].as_array().unwrap().iter().map(|v| v.as_u64().unwrap() as usize).collect();
    let org: Vec<usize> = m["cfa_origin"].as_array().unwrap().iter().map(|v| v.as_u64().unwrap() as usize).collect();
    let entries: Vec<ColorFilter> = cfa.iter().map(|v| ColorFilter::try_from(v.as_u64().unwrap() as u8).unwrap()).collect();
    let pat = CfaPattern { width: dim[1], height: dim[0], entries, origin_x: org[0], origin_y: org[1] };
    let bayer = ImageMetadata::with_cfa(w, h, SampleLayout::BayerCfa, pat.clone());
    if (dim[0], dim[1]) == (2, 2) && bayer.validate().is_ok() { bayer } else { ImageMetadata::with_cfa(w, h, SampleLayout::OtherCfa, pat) }
}

fn run(m: &Value) -> Value {
    let id = m["id"].as_str().unwrap();
    let (w, h) = (m["width"].as_u64().unwrap() as usize, m["height"].as_u64().unwrap() as usize);
    let bd = m["bit_depth"].as_u64().unwrap() as u8;
    let bytes = std::fs::read(format!("data/praw1/canon/{id}.u16")).unwrap();
    assert_eq!(bytes.len(), w * h * 2);
    let samples: Vec<u16> = bytes.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
    drop(bytes);
    let meta = meta_of(m, w, h);
    let t = Instant::now();
    let enc = pkon::encode_raw(&samples, w, h, bd, meta).unwrap();
    let enc_s = t.elapsed().as_secs_f64();
    let t = Instant::now();
    let dec = pkon::decode(&enc).unwrap();
    let dec_s = t.elapsed().as_secs_f64();
    let Decoded::Raw(r) = dec else { panic!("not RAW") };
    let hdr = pkon::parse_header(&enc).unwrap();
    let row = json!({
        "id": id, "bytes": enc.len(), "sha256": format!("{:x}", Sha256::digest(&enc)), "bit_exact": r.samples == samples,
        "meta_version": hdr.meta_version, "enc_s": enc_s, "dec_s": dec_s,
    });
    println!("{row}");
    row
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let manifest: Value = serde_json::from_slice(&std::fs::read(&a[1]).unwrap()).unwrap();
    let threads: usize = a.get(3).map_or(4, |s| s.parse().unwrap());
    let only: Option<Vec<String>> = a.get(4).map(|s| s.split(',').map(String::from).collect());
    let items: Vec<&Value> = manifest.as_array().unwrap().iter().filter(|m| only.as_ref().map_or(true, |l| l.iter().any(|x| x == m["id"].as_str().unwrap()))).collect();
    let next = Mutex::new(0usize);
    let rows = Mutex::new(vec![]);
    std::thread::scope(|s| {
        for _ in 0..threads {
            s.spawn(|| loop {
                let i = {
                    let mut n = next.lock().unwrap();
                    *n += 1;
                    *n - 1
                };
                let Some(m) = items.get(i) else { break };
                let row = run(m);
                rows.lock().unwrap().push(row);
            });
        }
    });
    let mut rows = rows.into_inner().unwrap();
    rows.sort_by_key(|r| r["id"].as_str().unwrap().to_string());
    std::fs::write(&a[2], serde_json::to_vec_pretty(&rows).unwrap()).unwrap();
}
