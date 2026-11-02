//! Experiment L1: RAW sample lattice normalization in front of the unchanged Lisa pipeline (isolated; nothing in src/ is touched).
//! For a whole image: origin = min(samples), g = gcd(v - origin) over every sample of the mosaic (all colour planes together,
//! because the production pipeline carries a single bit depth for the mosaic). g > 1 -> encode (v-origin)/g at the smallest bit
//! depth that holds it, through the production `pkon::encode_raw`; g == 1 -> call `pkon::encode_raw` on the original untouched.
//! The parameters live in an experimental 5-byte sidecar (origin u16, g u16, original bit depth u8) that is counted in the size.
//! Usage: exp_lisa_lattice <dataset_manifest.json> <jxl_per_image.csv> <out.json> [reps=2] [id,id,...]
use nanoscope::lisa::*;
use nanoscope::pkon::{self, Decoded};
use serde_json::{json, Value};
use std::time::Instant;

const SIDECAR_BYTES: usize = 5;

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

fn gcd(a: u32, b: u32) -> u32 {
    let (mut a, mut b) = (a, b);
    while b != 0 {
        (a, b) = (b, a % b);
    }
    a
}

/// (origin, g) of a sample slice; g == 1 for constant data.
fn lattice(s: &[u16]) -> (u16, u32) {
    let origin = *s.iter().min().unwrap();
    let mut g = 0u32;
    for &v in s {
        g = gcd(g, u32::from(v - origin));
        if g == 1 {
            break;
        }
    }
    (origin, g.max(1))
}

struct Exp {
    bytes: Vec<u8>,
    sidecar: Option<(u16, u16, u8)>, // (origin, g, original bit depth) when normalized
}

impl Exp {
    fn total(&self) -> usize {
        self.bytes.len() + if self.sidecar.is_some() { SIDECAR_BYTES } else { 0 }
    }
}

fn encode_exp(samples: &[u16], w: usize, h: usize, bd: u8, meta: &ImageMetadata) -> Exp {
    let (origin, g) = lattice(samples);
    if g == 1 {
        return Exp { bytes: pkon::encode_raw(samples, w, h, bd, meta.clone()).unwrap(), sidecar: None };
    }
    let norm: Vec<u16> = samples.iter().map(|&v| ((u32::from(v - origin)) / g) as u16).collect();
    let max = *norm.iter().max().unwrap();
    let nbd = ((32 - u32::from(max).leading_zeros()).max(1)) as u8;
    Exp { bytes: pkon::encode_raw(&norm, w, h, nbd, meta.clone()).unwrap(), sidecar: Some((origin, g as u16, bd)) }
}

fn decode_exp(e: &Exp) -> Vec<u16> {
    let Decoded::Raw(r) = pkon::decode(&e.bytes).unwrap() else { panic!("not RAW") };
    match e.sidecar {
        None => r.samples,
        Some((origin, g, _)) => r.samples.iter().map(|&k| (u32::from(k) * u32::from(g) + u32::from(origin)) as u16).collect(),
    }
}

fn jxl_bytes(csv: &str, id: &str) -> Option<u64> {
    let mut lines = csv.lines();
    let head: Vec<&str> = lines.next()?.split(',').collect();
    let col = head.iter().position(|c| *c == "jxl_bytes")?;
    lines.find(|l| l.starts_with(&format!("{id},"))).map(|l| l.split(',').nth(col).unwrap().parse().unwrap())
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let manifest: Value = serde_json::from_slice(&std::fs::read(&a[1]).unwrap()).unwrap();
    let csv = std::fs::read_to_string(&a[2]).unwrap();
    let reps: usize = a.get(4).map_or(2, |s| s.parse().unwrap());
    let only: Option<Vec<String>> = a.get(5).map(|s| s.split(',').map(String::from).collect());
    let mut rows = vec![];
    for m in manifest.as_array().unwrap() {
        let id = m["id"].as_str().unwrap();
        if only.as_ref().map_or(false, |l| !l.iter().any(|x| x == id)) {
            continue;
        }
        let (w, h) = (m["width"].as_u64().unwrap() as usize, m["height"].as_u64().unwrap() as usize);
        let bd = m["bit_depth"].as_u64().unwrap() as u8;
        let path = format!("data/praw1/canon/{id}.u16");
        let bytes = std::fs::read(&path).unwrap();
        assert_eq!(bytes.len(), w * h * 2);
        let samples: Vec<u16> = bytes.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
        drop(bytes);
        let meta = meta_of(m, w, h);
        let (origin, g) = lattice(&samples);
        // per-parity-plane lattices, informational only
        let plane: Vec<Value> = (0..4)
            .map(|i| {
                let p: Vec<u16> = (0..h).filter(|y| y & 1 == i >> 1).flat_map(|y| (0..w).filter(move |x| x & 1 == i & 1).map(move |x| (x, y))).map(|(x, y)| samples[y * w + x]).collect();
                let (o, g) = lattice(&p);
                json!([o, g])
            })
            .collect();
        let (mut tb, mut te) = (f64::MAX, f64::MAX);
        let (mut base, mut exp) = (vec![], None);
        for r in 0..reps {
            let t = Instant::now();
            let b = std::hint::black_box(pkon::encode_raw(&samples, w, h, bd, meta.clone()).unwrap());
            tb = tb.min(t.elapsed().as_secs_f64());
            let t = Instant::now();
            let e = std::hint::black_box(encode_exp(&samples, w, h, bd, &meta));
            te = te.min(t.elapsed().as_secs_f64());
            if r == 0 {
                base = b;
                exp = Some(e);
            }
        }
        let exp = exp.unwrap();
        let t = Instant::now();
        let back = decode_exp(&exp);
        let td = t.elapsed().as_secs_f64();
        let exact = back == samples;
        let identical = exp.sidecar.is_none() && exp.bytes == base;
        let jxl = jxl_bytes(&csv, id);
        let row = json!({
            "id": id, "bit_depth": bd, "samples": w * h, "origin": origin, "g": g, "plane_lattices_origin_g": plane,
            "norm_bit_depth": exp.sidecar.map(|_| ((32 - ((samples.iter().map(|&v| u32::from(v - origin) / g).max().unwrap()).leading_zeros())).max(1))),
            "base_bytes": base.len(), "exp_bytes": exp.total(), "change_pct": 100.0 * (exp.total() as f64 / base.len() as f64 - 1.0),
            "byte_identical": identical, "bit_exact": exact, "jxl_bytes": jxl,
            "exp_vs_jxl_pct": jxl.map(|j| 100.0 * (exp.total() as f64 / j as f64 - 1.0)),
            "base_vs_jxl_pct": jxl.map(|j| 100.0 * (base.len() as f64 / j as f64 - 1.0)),
            "enc_base_s": tb, "enc_exp_s": te, "enc_overhead_pct": 100.0 * (te / tb - 1.0), "dec_exp_s": td,
        });
        println!("{row}");
        rows.push(row);
    }
    std::fs::write(&a[3], serde_json::to_vec_pretty(&rows).unwrap()).unwrap();
}
