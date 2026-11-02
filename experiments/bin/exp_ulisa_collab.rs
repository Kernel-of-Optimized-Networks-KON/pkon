//! EXPERIMENTAL, isolated: ULISA collaboration viability harness (Stages 1-2). Touches no production code.
//!
//! Derives green-difference planes offline and encodes them with the unchanged production Lisa / ELLIE implementations:
//!   Lisa  : public stage APIs (`lisa_bayer_plane::encode_stage2_with_mode` -> `classify_stage3` -> `encode_stage4`) so no
//!           hidden second L1 is applied; the production `pkon::encode_raw` is the L-O baseline.
//!   ELLIE : `EllieCodec` (production default profile) on the four image-parity planes.
//! Usage: exp_ulisa_collab <source_manifest.json> <split.json> <out.json> <threads> <trainval|id,id,..> [--final] [--components] [--controls]
use nanoscope::canon::{Canonical, Layout, Spec};
use nanoscope::ellie::{EllieCodec, Stage3Profile, Stage4Enc};
use nanoscope::lisa::{self, CfaPattern, ColorFilter, ImageMetadata, SampleLayout};
use nanoscope::lisa_bayer_plane as bayer;
use nanoscope::pkon::{self, CodecParams, Decoded, EllieRepr};
use serde_json::{json, Value};
use std::sync::Mutex;
use std::time::Instant;

// ------------------------------------------------------------------ L1 (replica of the private `Lattice::detect`)
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct L1 {
    pub origin: u16,
    pub g: u16,
    pub coding_bd: u8,
}

pub fn detect_l1(samples: &[u16], bd: u8) -> Option<L1> {
    let (mut lo, mut hi) = (u16::MAX, 0u16);
    for &v in samples {
        lo = lo.min(v);
        hi = hi.max(v);
    }
    if samples.is_empty() || u32::from(hi) >= 1u32 << bd {
        return None;
    }
    let mut g = 0u32;
    for &v in samples {
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
    let max_norm = u32::from(hi - lo) / g;
    Some(L1 { origin: lo, g: g as u16, coding_bd: (32 - max_norm.leading_zeros()).max(1) as u8 })
}

pub fn l1_forward(s: &[u16], l: L1) -> Vec<u16> {
    s.iter().map(|&v| (u32::from(v - l.origin) / u32::from(l.g)) as u16).collect()
}
pub fn l1_inverse(s: &[u16], l: L1) -> Vec<u16> {
    s.iter().map(|&v| (u32::from(v) * u32::from(l.g) + u32::from(l.origin)) as u16).collect()
}

// ------------------------------------------------------------------ green-difference transform
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Interp {
    V0,
    V1,
}

/// Colour of the four image-parity positions, index = (y&1)*2 + (x&1).
pub fn parity_colors(cfa: &CfaPattern) -> [ColorFilter; 4] {
    let mut c = [ColorFilter::Green; 4];
    for (i, e) in c.iter_mut().enumerate() {
        *e = cfa.color_at(i & 1, i >> 1);
    }
    c
}

fn is_rb(c: ColorFilter) -> bool {
    matches!(c, ColorFilter::Red | ColorFilter::Blue)
}

/// Deterministic Ghat at every R/B site (0 elsewhere). Four edge neighbours are always green in a Bayer mosaic. Borders reflect
/// the index (-1 -> +1, w -> w-2), which preserves site parity. Needs w,h >= 2 (else all zero: centering only, still reversible).
pub fn ghat_field(m: &[u16], w: usize, h: usize, col: &[ColorFilter; 4], interp: Interp) -> Vec<u16> {
    let mut out = vec![0u16; w * h];
    if w < 2 || h < 2 {
        return out;
    }
    for y in 0..h {
        let (ym, yp) = (if y > 0 { y - 1 } else { y + 1 }, if y + 1 < h { y + 1 } else { y - 1 });
        for x in 0..w {
            if !is_rb(col[(y & 1) * 2 + (x & 1)]) {
                continue;
            }
            let (xm, xp) = (if x > 0 { x - 1 } else { x + 1 }, if x + 1 < w { x + 1 } else { x - 1 });
            let (n, s, we, e) = (m[ym * w + x] as u32, m[yp * w + x] as u32, m[y * w + xm] as u32, m[y * w + xp] as u32);
            let avg4 = (n + s + we + e + 2) >> 2;
            out[y * w + x] = match interp {
                Interp::V0 => avg4,
                Interp::V1 => {
                    let (dh, dv) = (we.abs_diff(e), n.abs_diff(s));
                    if dh < dv {
                        (we + e + 1) >> 1
                    } else if dv < dh {
                        (n + s + 1) >> 1
                    } else {
                        avg4
                    }
                }
            } as u16;
        }
    }
    out
}

/// `c = (v - ghat + H) mod 2^d` at R/B sites, G untouched. Also returns the count of sites whose unwrapped value left [0, M].
pub fn forward_mod(n: &[u16], gh: &[u16], w: usize, col: &[ColorFilter; 4], d: u8) -> (Vec<u16>, u64) {
    let (m, hh) = ((1i32 << d) - 1, 1i32 << (d - 1));
    let (mut t, mut wraps) = (n.to_vec(), 0u64);
    for (i, v) in t.iter_mut().enumerate() {
        let (x, y) = (i % w, i / w);
        if is_rb(col[(y & 1) * 2 + (x & 1)]) {
            let u = *v as i32 - gh[i] as i32 + hh;
            wraps += (u < 0 || u > m) as u64;
            *v = (u & m) as u16;
        }
    }
    (t, wraps)
}

pub fn inverse_mod(t: &[u16], gh: &[u16], w: usize, col: &[ColorFilter; 4], d: u8) -> Vec<u16> {
    let (m, hh) = ((1i32 << d) - 1, 1i32 << (d - 1));
    let mut n = t.to_vec();
    for (i, v) in n.iter_mut().enumerate() {
        let (x, y) = (i % w, i / w);
        if is_rb(col[(y & 1) * 2 + (x & 1)]) {
            *v = ((*v as i32 + gh[i] as i32 - hh) & m) as u16;
        }
    }
    n
}

/// (min, max, gcd of (v-min)) over R/B sites of a mosaic.
pub fn rb_range_gcd(t: &[u16], w: usize, col: &[ColorFilter; 4]) -> (u16, u16, u32) {
    let sites = || t.iter().enumerate().filter(|(i, _)| is_rb(col[((i / w) & 1) * 2 + (i % w & 1)])).map(|(_, &v)| v);
    let (lo, hi) = sites().fold((u16::MAX, 0u16), |(a, b), v| (a.min(v), b.max(v)));
    let mut g = 0u32;
    for v in sites() {
        let (mut a, mut b) = (g, u32::from(v - lo));
        while b != 0 {
            (a, b) = (b, a % b);
        }
        g = a;
        if g == 1 {
            break;
        }
    }
    (lo, hi, g)
}

// ------------------------------------------------------------------ planes
fn geometry(w: usize, h: usize) -> [(usize, usize, usize, usize); 4] {
    let mut g = [(0, 0, 0, 0); 4];
    for (i, e) in g.iter_mut().enumerate() {
        let (ox, oy) = (i & 1, i >> 1);
        *e = (ox, oy, (w + 1 - ox) / 2, (h + 1 - oy) / 2);
    }
    g
}

pub fn split_planes(m: &[u16], w: usize, h: usize) -> Vec<Vec<u16>> {
    geometry(w, h)
        .iter()
        .map(|&(ox, oy, pw, ph)| (0..ph).flat_map(|v| (0..pw).map(move |u| (u, v))).map(|(u, v)| m[(oy + 2 * v) * w + ox + 2 * u]).collect())
        .collect()
}

pub fn merge_planes(p: &[Vec<u16>], w: usize, h: usize) -> Vec<u16> {
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

// ------------------------------------------------------------------ engine wrappers
const ELLIE_CONTAINER_OVERHEAD_PER_PLANE: u64 = 32 + 7 + 1; // header + (2+5*1) metadata + profile byte
const TRANSFORM_ID_BYTES: u64 = 1;

/// Lisa payload bytes of a Bayer mosaic via the public stage APIs (no L1 here), plus exactness check on request.
fn lisa_stage_payload(m: &[u16], w: usize, h: usize, d: u8, meta: ImageMetadata, verify: bool) -> (u64, f64, f64, bool) {
    let t = Instant::now();
    let s2 = bayer::encode_stage2_with_mode(m, w, h, d, meta, bayer::Stage1Mode::default()).unwrap();
    let s4 = lisa::encode_stage4(lisa::classify_stage3(s2).unwrap()).unwrap();
    let enc_s = t.elapsed().as_secs_f64();
    let len = s4.payload().len() as u64;
    if !verify {
        return (len, enc_s, 0.0, true);
    }
    let t = Instant::now();
    let back = bayer::decode_stage2(&lisa::decode_stage4(&s4).unwrap().into_stage2()).unwrap();
    (len, enc_s, t.elapsed().as_secs_f64(), back == m)
}

/// ELLIE per-plane payload bytes at coding depth `bits` (production default profile); returns (per-plane bytes, enc s, dec s, exact).
fn ellie_planes(planes: &[Vec<u16>], dims: &[(usize, usize)], bits: u8) -> (Vec<u64>, f64, f64, bool, Vec<Vec<u16>>) {
    let (mut lens, mut enc_s, mut dec_s, mut ok) = (vec![], 0.0, 0.0, true);
    let mut decoded = vec![];
    for (p, &(pw, ph)) in planes.iter().zip(dims) {
        let t = Instant::now();
        let mut codec = EllieCodec::with_profile(pw, ph, bits as u32, Stage3Profile::Bc255S11Tree).unwrap();
        let mut enc = Stage4Enc::with_capacity(pw * ph);
        codec.encode_planes(&[p.as_slice()], &mut enc).unwrap();
        let bytes = enc.finish().to_vec();
        enc_s += t.elapsed().as_secs_f64();
        lens.push(bytes.len() as u64);
        let t = Instant::now();
        let mut out = vec![0u16; pw * ph];
        let mut codec = EllieCodec::with_profile(pw, ph, bits as u32, Stage3Profile::Bc255S11Tree).unwrap();
        codec.decode_planes(&bytes, &mut [out.as_mut_slice()]).unwrap();
        dec_s += t.elapsed().as_secs_f64();
        ok &= &out == p;
        decoded.push(out);
    }
    (lens, enc_s, dec_s, ok, decoded)
}

// ------------------------------------------------------------------ corpus
fn meta_of(m: &Value, w: usize, h: usize) -> Option<ImageMetadata> {
    let cfa = m["cfa"].as_array().filter(|a| !a.is_empty())?;
    let dim: Vec<usize> = m["cfa_dim"].as_array()?.iter().map(|v| v.as_u64().unwrap() as usize).collect();
    let org: Vec<usize> = m["cfa_origin"].as_array()?.iter().map(|v| v.as_u64().unwrap() as usize).collect();
    let entries: Vec<ColorFilter> = cfa.iter().map(|v| ColorFilter::try_from(v.as_u64().unwrap() as u8).unwrap()).collect();
    let pat = CfaPattern { width: dim[1], height: dim[0], entries, origin_x: org[0], origin_y: org[1] };
    let bayer = ImageMetadata::with_cfa(w, h, SampleLayout::BayerCfa, pat);
    if (dim[0], dim[1]) == (2, 2) && bayer.validate().is_ok() { Some(bayer) } else { None }
}

fn role_names(col: &[ColorFilter; 4]) -> [&'static str; 4] {
    let mut seen_g = 0;
    let mut r = [""; 4];
    for i in 0..4 {
        r[i] = match col[i] {
            ColorFilter::Red => "R",
            ColorFilter::Blue => "B",
            _ => {
                seen_g += 1;
                if seen_g == 1 { "G1" } else { "G2" }
            }
        };
    }
    r
}

fn pstats(t: &[u16], w: usize, col: &[ColorFilter; 4]) -> Value {
    let (lo, hi, g) = rb_range_gcd(t, w, col);
    json!({"min": lo, "max": hi, "gcd": g})
}

fn run_image(m: &Value, components: bool) -> Value {
    let id = m["id"].as_str().unwrap();
    let (w, h) = (m["width"].as_u64().unwrap() as usize, m["height"].as_u64().unwrap() as usize);
    let bd = m["bit_depth"].as_u64().unwrap() as u8;
    let bytes = std::fs::read(format!("data/praw1/canon/{id}.u16")).unwrap();
    let samples: Vec<u16> = bytes.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
    drop(bytes);
    let meta = meta_of(m, w, h).expect("Bayer only");
    let col = parity_colors(meta.cfa.as_ref().unwrap());
    let roles = role_names(&col);
    let dims: Vec<(usize, usize)> = geometry(w, h).iter().map(|g| (g.2, g.3)).collect();

    // --- production baseline (L-O): pkon::encode_raw incl. internal L1
    let t = Instant::now();
    let file = pkon::encode_raw(&samples, w, h, bd, meta.clone()).unwrap();
    let prod_enc = t.elapsed().as_secs_f64();
    let hdr = pkon::parse_header(&file).unwrap();
    let prod_lat = match &hdr.params {
        CodecParams::Lisa(p) => p.lattice.map(|l| L1 { origin: l.origin, g: l.g, coding_bd: l.coding_bit_depth }),
        _ => panic!(),
    };
    let t = Instant::now();
    let dec = pkon::decode(&file).unwrap();
    let prod_dec = t.elapsed().as_secs_f64();
    let prod_exact = matches!(&dec, Decoded::Raw(r) if r.samples == samples);
    drop(dec);
    let l1 = detect_l1(&samples, bd);
    let l1_match = l1 == prod_lat;
    let (nb, d) = match l1 {
        Some(l) => (l1_forward(&samples, l), l.coding_bd),
        None => (samples.clone(), bd),
    };
    let side_l1: u64 = if l1.is_some() { 5 } else { 0 };
    let lisa_hdr_meta = (32 + hdr.meta_len as u64) - side_l1; // header + v1 metadata, without the lattice block
    // L1-off Lisa payload (stage APIs on raw samples at the header bit depth); equals production payload when there is no lattice
    let l1_off_payload = if l1.is_some() { lisa_stage_payload(&samples, w, h, bd, meta.clone(), false).0 } else { hdr.payload_len as u64 };

    let mut out = json!({
        "id": id, "w": w, "h": h, "bd": bd, "coding_bd": d, "l1": l1.map(|l| json!({"origin": l.origin, "g": l.g, "coding_bd": l.coding_bd})),
        "l1_matches_production": l1_match, "roles": roles,
        "L_O": {"file_bytes": file.len(), "payload": hdr.payload_len, "meta_version": hdr.meta_version, "enc_s": prod_enc, "dec_s": prod_dec, "exact": prod_exact,
                "l1_off_payload": l1_off_payload, "l1_off_file_bytes": lisa_hdr_meta + l1_off_payload},
    });

    // --- ELLIE original: normalized @ bits=d (fair arm) and production container @ 16 bit, no L1 (reference)
    let nplanes = split_planes(&nb, w, h);
    let (lens, es, ds, ok, _) = ellie_planes(&nplanes, &dims, d);
    out["E_O"] = json!({"plane_payload": lens, "bytes": lens.iter().sum::<u64>() + 4 * ELLIE_CONTAINER_OVERHEAD_PER_PLANE + side_l1, "enc_s": es, "dec_s": ds, "exact_planes": ok});
    {
        let raw_planes = split_planes(&samples, w, h);
        let (mut total, mut ok, mut es, mut ds) = (0u64, true, 0.0, 0.0);
        let mut per = vec![];
        for (p, &(pw, ph)) in raw_planes.iter().zip(&dims) {
            let data: Vec<u8> = p.iter().flat_map(|v| v.to_le_bytes()).collect();
            let c = Canonical::new(Spec { width: pw as u32, height: ph as u32, layout: Layout::Gray, bit_depth: 16 }, data).unwrap();
            let t = Instant::now();
            let f = pkon::encode_standard_with(&c, EllieRepr::Untransformed).unwrap();
            es += t.elapsed().as_secs_f64();
            let t = Instant::now();
            ok &= matches!(pkon::decode(&f).unwrap(), Decoded::Standard(r) if r.data == c.data);
            ds += t.elapsed().as_secs_f64();
            total += f.len() as u64;
            per.push(f.len());
        }
        out["E_O_prod16"] = json!({"file_bytes": total, "per_plane": per, "enc_s": es, "dec_s": ds, "exact": ok});
    }

    // --- transformed arms
    for (name, interp) in [("V0", Interp::V0), ("V1", Interp::V1)] {
        let t = Instant::now();
        let gh = ghat_field(&nb, w, h, &col, interp);
        let (tm, wraps) = forward_mod(&nb, &gh, w, &col, d);
        let tf_s = t.elapsed().as_secs_f64();
        let rb_sites = ((w * h) as u64) / 2;
        let derived = pstats(&tm, w, &col);
        let derived_l1 = detect_l1(&tm, d);
        let (lp, les, lds, lok) = lisa_stage_payload(&tm, w, h, d, meta.clone(), true);
        // inverse from the decoded mosaic (G first by construction: Ghat reads only G sites)
        let t = Instant::now();
        let gh2 = ghat_field(&tm, w, h, &col, interp);
        let n2 = inverse_mod(&tm, &gh2, w, &col, d);
        let inv_s = t.elapsed().as_secs_f64();
        let back = match l1 {
            Some(l) => l1_inverse(&n2, l),
            None => n2,
        };
        let recon = back == samples;
        let secondary = derived_l1.map(|l| {
            let f = pkon::encode_raw(&tm, w, h, d, meta.clone()).unwrap();
            json!({"origin": l.origin, "g": l.g, "coding_bd": l.coding_bd, "encode_raw_file_bytes": f.len()})
        });
        let tplanes = split_planes(&tm, w, h);
        let (lens, es, ds, ok, dec_planes) = ellie_planes(&tplanes, &dims, d);
        let merged = merge_planes(&dec_planes, w, h);
        let e_recon = {
            let g2 = ghat_field(&merged, w, h, &col, interp);
            let n2 = inverse_mod(&merged, &g2, w, &col, d);
            (match l1 {
                Some(l) => l1_inverse(&n2, l),
                None => n2,
            }) == samples
        };
        out[format!("L_T_{name}")] = json!({"payload": lp, "bytes": lisa_hdr_meta + side_l1 + TRANSFORM_ID_BYTES + lp, "enc_s": les, "dec_s": lds, "transform_s": tf_s, "inverse_s": inv_s,
            "exact_stream": lok, "exact_original": recon, "wrap_rate": wraps as f64 / rb_sites as f64, "derived_rb": derived, "secondary_l1": secondary});
        out[format!("E_T_{name}")] = json!({"plane_payload": lens, "bytes": lens.iter().sum::<u64>() + 4 * ELLIE_CONTAINER_OVERHEAD_PER_PLANE + side_l1 + TRANSFORM_ID_BYTES,
            "enc_s": es, "dec_s": ds, "transform_s": tf_s, "exact_planes": ok, "exact_original": e_recon});

        if components {
            // separate Lisa component streams (Monochrome stage path): measures the cost of splitting the shared-context joint stream
            let mut per_o = vec![];
            let mut per_t = vec![];
            let mut ok_all = true;
            for (i, &(pw, ph)) in dims.iter().enumerate() {
                let mm = ImageMetadata::without_cfa(pw, ph, SampleLayout::Monochrome);
                if name == "V0" {
                    let (p, _, _, k) = lisa_stage_payload(&nplanes[i], pw, ph, d, mm.clone(), true);
                    per_o.push(p);
                    ok_all &= k;
                }
                let (p, _, _, k) = lisa_stage_payload(&tplanes[i], pw, ph, d, mm, true);
                per_t.push(p);
                ok_all &= k;
            }
            out[format!("L_comp_{name}")] = json!({"orig_planes": per_o, "trans_planes": per_t, "exact": ok_all});
        }
    }
    out
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let flag = |f: &str| a.iter().any(|x| x == f);
    let manifest: Value = serde_json::from_slice(&std::fs::read(&a[1]).unwrap()).unwrap();
    let split: Value = serde_json::from_slice(&std::fs::read(&a[2]).unwrap()).unwrap();
    let threads: usize = a[4].parse().unwrap();
    let in_split = |s: &str, id: &str| split[s].as_array().unwrap().iter().any(|v| v == id);
    let ids: Vec<String> = if a[5] == "trainval" {
        manifest.as_array().unwrap().iter().map(|m| m["id"].as_str().unwrap().to_string()).filter(|i| in_split("train", i) || in_split("val", i)).collect()
    } else {
        a[5].split(',').map(String::from).collect()
    };
    for id in &ids {
        if in_split("test", id) && !flag("--final") {
            eprintln!("REFUSED: {id} is in the held-out test split; Stage 3 needs separate authorization (--final)");
            std::process::exit(2);
        }
    }
    let mut items: Vec<&Value> = manifest.as_array().unwrap().iter().filter(|m| ids.iter().any(|i| i == m["id"].as_str().unwrap()) && meta_of(m, m["width"].as_u64().unwrap() as usize, m["height"].as_u64().unwrap() as usize).is_some()).collect();
    items.sort_by_key(|m| std::cmp::Reverse(m["width"].as_u64().unwrap() * m["height"].as_u64().unwrap()));
    let components = flag("--components");
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
                let r = run_image(m, components);
                eprintln!("done {}", r["id"]);
                rows.lock().unwrap().push(r);
            });
        }
    });
    let mut rows = rows.into_inner().unwrap();
    rows.sort_by_key(|r| r["id"].as_str().unwrap().to_string());
    let out = json!({"threads": threads, "images": rows});
    std::fs::write(&a[3], serde_json::to_vec_pretty(&out).unwrap()).unwrap();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfa(entries: [u8; 4], ox: usize, oy: usize) -> CfaPattern {
        CfaPattern { width: 2, height: 2, entries: entries.iter().map(|&c| ColorFilter::try_from(c).unwrap()).collect(), origin_x: ox, origin_y: oy }
    }
    fn rng(seed: &mut u64) -> u32 {
        *seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        (*seed >> 33) as u32
    }
    const PATTERNS: [[u8; 4]; 3] = [[0, 1, 1, 2], [2, 1, 1, 0], [1, 2, 0, 1]]; // RGGB, BGGR, GBRG

    #[test]
    fn neighbours_of_rb_sites_are_green_for_every_pattern_origin_and_odd_size() {
        for p in PATTERNS {
            for (ox, oy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                let c = cfa(p, ox, oy);
                let col = parity_colors(&c);
                for (w, h) in [(2usize, 2usize), (5, 3), (4, 7), (9, 9)] {
                    for y in 0..h {
                        for x in 0..w {
                            if !is_rb(c.color_at(x, y)) {
                                continue;
                            }
                            assert!(is_rb(col[(y & 1) * 2 + (x & 1)]));
                            let xs = [if x > 0 { x - 1 } else { x + 1 }, if x + 1 < w { x + 1 } else { x - 1 }];
                            let ys = [if y > 0 { y - 1 } else { y + 1 }, if y + 1 < h { y + 1 } else { y - 1 }];
                            for &xx in &xs {
                                assert_eq!(c.color_at(xx, y), ColorFilter::Green);
                            }
                            for &yy in &ys {
                                assert_eq!(c.color_at(x, yy), ColorFilter::Green);
                            }
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn modular_roundtrip_exact_all_patterns_depths_variants_and_extremes() {
        let mut seed = 7;
        for p in PATTERNS {
            for (ox, oy) in [(0, 0), (1, 1), (1, 0)] {
                let c = cfa(p, ox, oy);
                let col = parity_colors(&c);
                for d in [1u8, 8, 10, 14, 15, 16] {
                    let m = ((1u32 << d) - 1) as u16;
                    for (w, h) in [(2usize, 2usize), (7, 5), (6, 9), (16, 16)] {
                        for kind in 0..4 {
                            let n: Vec<u16> = (0..w * h)
                                .map(|i| match kind {
                                    0 => (rng(&mut seed) % (m as u32 + 1)) as u16,
                                    1 => if (i + i / w) % 2 == 0 { 0 } else { m }, // R=0,G=max and R=max,G=0 extremes
                                    2 => if (i + i / w) % 2 == 0 { m } else { 0 },
                                    _ => m / 2,
                                })
                                .collect();
                            for interp in [Interp::V0, Interp::V1] {
                                let gh = ghat_field(&n, w, h, &col, interp);
                                let (t, _) = forward_mod(&n, &gh, w, &col, d);
                                assert!(t.iter().all(|&v| v <= m), "range d={d}");
                                for i in 0..w * h {
                                    if !is_rb(col[((i / w) & 1) * 2 + (i % w & 1)]) {
                                        assert_eq!(t[i], n[i], "G sites must be untouched");
                                    }
                                }
                                let gh2 = ghat_field(&t, w, h, &col, interp);
                                assert_eq!(gh, gh2, "decoder reproduces Ghat from G only");
                                assert_eq!(inverse_mod(&t, &gh2, w, &col, d), n);
                            }
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn degenerate_sizes_stay_reversible() {
        let col = parity_colors(&cfa(PATTERNS[0], 0, 0));
        for (w, h) in [(1usize, 1usize), (1, 5), (5, 1)] {
            let n: Vec<u16> = (0..w * h).map(|i| (i * 977 % 4096) as u16).collect();
            let gh = ghat_field(&n, w, h, &col, Interp::V0);
            let (t, _) = forward_mod(&n, &gh, w, &col, 12);
            assert_eq!(inverse_mod(&t, &gh, w, &col, 12), n);
        }
    }

    #[test]
    fn ghat_values_match_hand_computed() {
        // RGGB 4x4, interior R at (2,2): N=(2,1) S=(2,3) W=(1,2) E=(3,2)
        let col = parity_colors(&cfa(PATTERNS[0], 0, 0));
        let mut n = vec![0u16; 16];
        n[1 * 4 + 2] = 10;
        n[3 * 4 + 2] = 20;
        n[2 * 4 + 1] = 30;
        n[2 * 4 + 3] = 41;
        assert_eq!(ghat_field(&n, 4, 4, &col, Interp::V0)[2 * 4 + 2], (10 + 20 + 30 + 41 + 2) >> 2);
        // |W-E| = 11 < |N-S| ... equal-ish: dv=10, dh=11 -> vertical is smoother
        assert_eq!(ghat_field(&n, 4, 4, &col, Interp::V1)[2 * 4 + 2], (10 + 20 + 1) >> 1);
        // reflected border: R at (0,0): N->S(0,1), W->E(1,0)
        n[1 * 4 + 0] = 8;
        n[0 * 4 + 1] = 12;
        assert_eq!(ghat_field(&n, 4, 4, &col, Interp::V0)[0], (8 + 8 + 12 + 12 + 2) >> 2);
    }

    #[test]
    fn l1_replica_roundtrips_and_shapes_the_depth() {
        let s: Vec<u16> = (0..256u32).map(|i| (800 + 16 * (i % 50)) as u16).collect();
        let l = detect_l1(&s, 14).unwrap();
        assert_eq!((l.origin, l.g), (800, 16));
        assert_eq!(l1_inverse(&l1_forward(&s, l), l), s);
        assert!(detect_l1(&[1, 2, 3], 8).is_none());
    }

    #[test]
    fn engines_roundtrip_a_derived_mosaic() {
        let (w, h, d) = (40usize, 30usize, 12u8);
        let c = cfa(PATTERNS[2], 0, 0);
        let col = parity_colors(&c);
        let meta = ImageMetadata::with_cfa(w, h, SampleLayout::BayerCfa, c);
        let mut seed = 3;
        let n: Vec<u16> = (0..w * h).map(|i| (((i % w) * 40 + (i / w) * 20) as u32 % 4000 + rng(&mut seed) % 30) as u16).collect();
        let gh = ghat_field(&n, w, h, &col, Interp::V0);
        let (t, _) = forward_mod(&n, &gh, w, &col, d);
        let (_, _, _, ok) = lisa_stage_payload(&t, w, h, d, meta, true);
        assert!(ok);
        let planes = split_planes(&t, w, h);
        let dims: Vec<_> = geometry(w, h).iter().map(|g| (g.2, g.3)).collect();
        let (_, _, _, ok, dec) = ellie_planes(&planes, &dims, d);
        assert!(ok);
        assert_eq!(merge_planes(&dec, w, h), t);
        assert_eq!(merge_planes(&split_planes(&n, w, h), w, h), n);
    }
}
