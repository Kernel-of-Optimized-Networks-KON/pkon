//! EXPERIMENTAL, isolated: ULISA bit-split. Lisa (production `encode_raw`) codes the high bits `n >> k` of the L1-normalized
//! mosaic; ELLIE codes the low k bits as four parity planes. Streams share nothing. Touches no production code.
//! Usage: exp_ulisa_split <source_manifest.json> <split.json> <out.json> <threads> <trainval|test|all|id,id> <k,k,..>
use nanoscope::ellie::{EllieCodec, Stage3Profile, Stage4Enc};
use nanoscope::lisa::{CfaPattern, ColorFilter, ImageMetadata, SampleLayout};
use nanoscope::pkon::{self, CodecParams, Decoded};
use serde_json::{json, Value};
use std::sync::Mutex;
use std::time::Instant;

const ELLIE_PLANE_OVERHEAD: u64 = 40; // container-equivalent per plane (as in exp_ulisa_collab)
const COMBINED_META: u64 = 16; // k, 4 plane lengths, termination, container tag

fn detect_l1(s: &[u16], bd: u8) -> Option<(u16, u16, u8)> {
    let (mut lo, mut hi) = (u16::MAX, 0u16);
    for &v in s {
        lo = lo.min(v);
        hi = hi.max(v);
    }
    if s.is_empty() || u32::from(hi) >= 1u32 << bd {
        return None;
    }
    let mut g = 0u32;
    for &v in s {
        let (mut a, mut b) = (g, u32::from(v - lo));
        while b != 0 {
            (a, b) = (b, a % b);
        }
        g = a;
        if g == 1 {
            return None;
        }
    }
    if g < 2 {
        return None;
    }
    let mx = u32::from(hi - lo) / g;
    Some((lo, g as u16, (32 - mx.leading_zeros()).max(1) as u8))
}

fn geometry(w: usize, h: usize) -> [(usize, usize, usize, usize); 4] {
    let mut g = [(0, 0, 0, 0); 4];
    for (i, e) in g.iter_mut().enumerate() {
        let (ox, oy) = (i & 1, i >> 1);
        *e = (ox, oy, (w + 1 - ox) / 2, (h + 1 - oy) / 2);
    }
    g
}
fn split_planes(m: &[u16], w: usize, h: usize) -> Vec<Vec<u16>> {
    geometry(w, h)
        .iter()
        .map(|&(ox, oy, pw, ph)| (0..ph).flat_map(|v| (0..pw).map(move |u| (u, v))).map(|(u, v)| m[(oy + 2 * v) * w + ox + 2 * u]).collect())
        .collect()
}
fn merge_planes(p: &[Vec<u16>], w: usize, h: usize) -> Vec<u16> {
    let mut m = vec![0u16; w * h];
    for (i, &(ox, oy, pw, ph)) in geometry(w, h).iter().enumerate() {
        for v in 0..ph {
            for u in 0..pw {
                m[(oy + 2 * v) * w + ox + 2 * u] = p[i][v * pw + u];
            }
        }
    }
    m
}

fn meta_of(m: &Value, w: usize, h: usize) -> Option<ImageMetadata> {
    let cfa = m["cfa"].as_array().filter(|a| !a.is_empty())?;
    let dim: Vec<usize> = m["cfa_dim"].as_array()?.iter().map(|v| v.as_u64().unwrap() as usize).collect();
    let org: Vec<usize> = m["cfa_origin"].as_array()?.iter().map(|v| v.as_u64().unwrap() as usize).collect();
    let entries: Vec<ColorFilter> = cfa.iter().map(|v| ColorFilter::try_from(v.as_u64().unwrap() as u8).unwrap()).collect();
    let pat = CfaPattern { width: dim[1], height: dim[0], entries, origin_x: org[0], origin_y: org[1] };
    let b = ImageMetadata::with_cfa(w, h, SampleLayout::BayerCfa, pat);
    if (dim[0], dim[1]) == (2, 2) && b.validate().is_ok() {
        Some(b)
    } else {
        None
    }
}

/// ELLIE low-bit planes -> (total payload+overhead, enc s, dec s, decoded planes)
fn ellie_low(planes: &[Vec<u16>], dims: &[(usize, usize)], k: u8) -> (u64, f64, f64, Vec<Vec<u16>>) {
    let (mut total, mut es, mut ds, mut dec) = (0u64, 0.0, 0.0, vec![]);
    for (p, &(pw, ph)) in planes.iter().zip(dims) {
        let t = Instant::now();
        let mut c = EllieCodec::with_profile(pw, ph, k as u32, Stage3Profile::Bc255S11Tree).unwrap();
        let mut enc = Stage4Enc::with_capacity(pw * ph);
        c.encode_planes(&[p.as_slice()], &mut enc).unwrap();
        let bytes = enc.finish().to_vec();
        es += t.elapsed().as_secs_f64();
        total += bytes.len() as u64 + ELLIE_PLANE_OVERHEAD;
        let t = Instant::now();
        let mut out = vec![0u16; pw * ph];
        let mut c = EllieCodec::with_profile(pw, ph, k as u32, Stage3Profile::Bc255S11Tree).unwrap();
        c.decode_planes(&bytes, &mut [out.as_mut_slice()]).unwrap();
        ds += t.elapsed().as_secs_f64();
        dec.push(out);
    }
    (total, es, ds, dec)
}

fn run_image(m: &Value, ks: &[u8]) -> Value {
    let id = m["id"].as_str().unwrap();
    let (w, h) = (m["width"].as_u64().unwrap() as usize, m["height"].as_u64().unwrap() as usize);
    let bd = m["bit_depth"].as_u64().unwrap() as u8;
    let bytes = std::fs::read(format!("data/praw1/canon/{id}.u16")).unwrap();
    let samples: Vec<u16> = bytes.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
    drop(bytes);
    let meta = meta_of(m, w, h).expect("Bayer only");
    let dims: Vec<(usize, usize)> = geometry(w, h).iter().map(|g| (g.2, g.3)).collect();

    let t = Instant::now();
    let file = pkon::encode_raw(&samples, w, h, bd, meta.clone()).unwrap();
    let prod_enc = t.elapsed().as_secs_f64();
    let t = Instant::now();
    let dec = pkon::decode(&file).unwrap();
    let prod_dec = t.elapsed().as_secs_f64();
    let prod_exact = matches!(&dec, Decoded::Raw(r) if r.samples == samples);
    drop(dec);
    let hdr = pkon::parse_header(&file).unwrap();
    let prod_l1 = match &hdr.params {
        CodecParams::Lisa(p) => p.lattice.is_some(),
        _ => false,
    };

    // The split operates on the L1-normalized mosaic (Lisa's L1 stays authoritative: replica must agree with production).
    let l1 = detect_l1(&samples, bd);
    assert_eq!(l1.is_some(), prod_l1, "L1 replica disagrees with production");
    let (nb, d, side) = match l1 {
        Some((o, g, cd)) => (samples.iter().map(|&v| (u32::from(v - o) / u32::from(g)) as u16).collect::<Vec<_>>(), cd, 5u64),
        None => (samples.clone(), bd, 0),
    };
    let mut out = json!({"id": id, "bd": bd, "coding_bd": d, "l1": l1.is_some(), "prod_bytes": file.len(), "prod_enc_s": prod_enc, "prod_dec_s": prod_dec, "prod_exact": prod_exact, "splits": {}});
    for &k in ks {
        if k >= d {
            continue;
        }
        let mask = (1u16 << k) - 1;
        let hi: Vec<u16> = nb.iter().map(|&v| v >> k).collect();
        let lo: Vec<u16> = nb.iter().map(|&v| v & mask).collect();
        let t = Instant::now();
        let lf = pkon::encode_raw(&hi, w, h, d - k, meta.clone()).unwrap();
        let l_enc = t.elapsed().as_secs_f64();
        let t = Instant::now();
        let lhi = match pkon::decode(&lf).unwrap() {
            Decoded::Raw(r) => r.samples,
            _ => panic!(),
        };
        let l_dec = t.elapsed().as_secs_f64();
        let lplanes = split_planes(&lo, w, h);
        let (eb, e_enc, e_dec, dplanes) = ellie_low(&lplanes, &dims, k);
        let lolo = merge_planes(&dplanes, w, h);
        let recon_n: Vec<u16> = lhi.iter().zip(&lolo).map(|(&a, &b)| (a << k) | b).collect();
        let recon: Vec<u16> = match l1 {
            Some((o, g, _)) => recon_n.iter().map(|&v| (u32::from(v) * u32::from(g) + u32::from(o)) as u16).collect(),
            None => recon_n,
        };
        let total = lf.len() as u64 + eb + side + COMBINED_META;
        out["splits"][k.to_string()] = json!({"lisa_hi": lf.len(), "ellie_lo": eb, "total": total, "exact": recon == samples,
            "lisa_enc_s": l_enc, "lisa_dec_s": l_dec, "ellie_enc_s": e_enc, "ellie_dec_s": e_dec});
    }
    out
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let manifest: Value = serde_json::from_slice(&std::fs::read(&a[1]).unwrap()).unwrap();
    let split: Value = serde_json::from_slice(&std::fs::read(&a[2]).unwrap()).unwrap();
    let threads: usize = a[4].parse().unwrap();
    let ks: Vec<u8> = a[6].split(',').map(|s| s.parse().unwrap()).collect();
    let in_split = |s: &str, id: &str| split[s].as_array().unwrap().iter().any(|v| v == id);
    let sel = |id: &str| match a[5].as_str() {
        "trainval" => in_split("train", id) || in_split("val", id),
        "test" => in_split("test", id),
        "all" => true,
        l => l.split(',').any(|x| x == id),
    };
    let mut items: Vec<&Value> = manifest
        .as_array()
        .unwrap()
        .iter()
        .filter(|m| sel(m["id"].as_str().unwrap()) && meta_of(m, m["width"].as_u64().unwrap() as usize, m["height"].as_u64().unwrap() as usize).is_some())
        .collect();
    items.sort_by_key(|m| std::cmp::Reverse(m["width"].as_u64().unwrap() * m["height"].as_u64().unwrap()));
    let next = Mutex::new(0usize);
    let rows = Mutex::new(vec![]);
    std::thread::scope(|s| {
        for _ in 0..threads {
            s.spawn(|| loop {
                let i = {
                    let mut n = next.lock().unwrap();
                    let i = *n;
                    *n += 1;
                    i
                };
                let Some(m) = items.get(i) else { return };
                let r = run_image(m, &ks);
                eprintln!("done {}", r["id"]);
                rows.lock().unwrap().push(r);
            });
        }
    });
    let mut rows = rows.into_inner().unwrap();
    rows.sort_by_key(|r| r["id"].as_str().unwrap().to_string());
    std::fs::write(&a[3], serde_json::to_vec_pretty(&json!({"threads": threads, "images": rows})).unwrap()).unwrap();
}
