//! Deja Vu Shrub: adaptive H/V/D predictor weighting vs. the production ELLIE Stage 1, three-image canary.
//!   exp_ellie_dvs <out_dir> <reps>
//! Baseline = production EllieCodec (Bc255S11); experiment = s1d::Stage1D + production Stage 2/3/4. Complete-file bytes =
//! production container overhead (measured from pkon::encode_standard_with per representation) + plane stream bytes.
#[path = "s1d.rs"]
mod s1d;
use nanoscope::canon::{Canonical, Spec};
use nanoscope::ellie::{self, EllieCodec, Stage2, Stage3, Stage3Profile, Stage4Dec, Stage4Enc};
use nanoscope::pkon::{self, EllieRepr};
use s1d::{Cfg, Stage1D};
use std::time::Instant;

const CROP: u32 = 1024;

fn crop(c: Canonical) -> Canonical {
    let Spec { width, height, layout, bit_depth } = c.spec;
    let (nw, nh) = (width.min(CROP), height.min(CROP));
    if (nw, nh) == (width, height) {
        return c;
    }
    let (x0, y0) = ((width - nw) as usize / 2, (height - nh) as usize / 2);
    let ch = layout.channels();
    let mut data = Vec::with_capacity(nw as usize * nh as usize * ch);
    for y in 0..nh as usize {
        let s = ((y0 + y) * width as usize + x0) * ch;
        data.extend_from_slice(&c.data[s..s + nw as usize * ch]);
    }
    let mut out = Canonical::new(Spec { width: nw, height: nh, layout, bit_depth }, data).unwrap();
    out.notes = c.notes;
    out
}

fn planes_of(img: &Canonical, ycocg: bool) -> (Vec<Vec<u16>>, Vec<u32>) {
    let ch = img.spec.layout.channels();
    let n = img.spec.width as usize * img.spec.height as usize;
    let mut planes = vec![vec![0u16; n]; ch];
    for (i, px) in img.data.chunks_exact(ch).enumerate() {
        for (k, &v) in px.iter().enumerate() {
            planes[k][i] = v as u16;
        }
    }
    let mut bits = vec![8u32; ch];
    if ycocg {
        assert!(ellie::ycocg_r_forward_planes(&mut planes[..3]));
        bits[1] = 9;
        bits[2] = 9;
    }
    (planes, bits)
}

fn base_encode(planes: &[Vec<u16>], bits: &[u32], w: usize, h: usize) -> Vec<Vec<u8>> {
    let mut enc = Stage4Enc::with_capacity(w * h);
    let mut out = vec![];
    for (p, &b) in planes.iter().zip(bits) {
        let mut c = EllieCodec::with_profile(w, h, b, Stage3Profile::Bc255S11).unwrap();
        enc.reset();
        c.encode_planes(&[p.as_slice()], &mut enc).unwrap();
        out.push(enc.finish().to_vec());
    }
    out
}

fn base_decode(streams: &[Vec<u8>], bits: &[u32], w: usize, h: usize) -> Vec<Vec<u16>> {
    streams
        .iter()
        .zip(bits)
        .map(|(s, &b)| {
            let mut c = EllieCodec::with_profile(w, h, b, Stage3Profile::Bc255S11).unwrap();
            let mut p = vec![0u16; w * h];
            c.decode_planes(s, &mut [p.as_mut_slice()]).unwrap();
            p
        })
        .collect()
}

fn dvs_encode(cfg: Cfg, planes: &[Vec<u16>], bits: &[u32], w: usize, h: usize) -> Vec<Vec<u8>> {
    let mut enc = Stage4Enc::with_capacity(w * h);
    let mut out = vec![];
    let mut res = vec![0i32; w * h];
    for (p, &b) in planes.iter().zip(bits) {
        let mut s1 = Stage1D::new(w, h, b, cfg).unwrap();
        let mut s3 = Stage3::with_profile(w, h, b, Stage3Profile::Bc255S11).unwrap();
        s3.reset();
        enc.reset();
        s1.scan(|i, pr| {
            res[i] = p[i] as i32 - pr as i32;
            p[i] as u32
        });
        Stage2::new().encode_in_place(&mut res);
        s3.encode_plane(&res, &mut enc).unwrap();
        out.push(enc.finish().to_vec());
    }
    out
}

fn dvs_decode(cfg: Cfg, streams: &[Vec<u8>], bits: &[u32], w: usize, h: usize) -> Vec<Vec<u16>> {
    streams
        .iter()
        .zip(bits)
        .map(|(s, &b)| {
            let mut s1 = Stage1D::new(w, h, b, cfg).unwrap();
            let mut s3 = Stage3::with_profile(w, h, b, Stage3Profile::Bc255S11).unwrap();
            s3.reset();
            let mut dec = Stage4Dec::new(s);
            let mut res = vec![0i32; w * h];
            s3.decode_plane(&mut res, &mut dec).unwrap();
            Stage2::new().decode_in_place(&mut res);
            let m = s1.max_value() as i32;
            let mut out = vec![0u16; w * h];
            s1.scan(|i, pr| {
                let v = (pr as i32 + res[i]).clamp(0, m);
                out[i] = v as u16;
                v as u32
            });
            out
        })
        .collect()
}

fn tot(s: &[Vec<u8>]) -> usize {
    s.iter().map(|v| v.len()).sum()
}

fn main() {
    if cfg!(debug_assertions) {
        eprintln!("use --release");
        std::process::exit(2);
    }
    let a: Vec<String> = std::env::args().collect();
    let dir = std::path::PathBuf::from(&a[1]);
    let reps: usize = a[2].parse().unwrap();
    std::fs::create_dir_all(&dir).unwrap();
    let c = |shrub, shift, c, paeth| Cfg { shrub, shift, c, paeth };
    // (label, cfg); None = production EllieCodec.
    let variants: Vec<(&str, Option<Cfg>)> = vec![
        ("prod(Paeth)", None),
        ("noPaeth", Some(c(false, 4, 64, false))),
        ("curDVS r16 C64 +Paeth", Some(c(true, 4, 64, true))),
        ("r8 C64 +Paeth", Some(c(true, 3, 64, true))),
        ("r8 C32 +Paeth", Some(c(true, 3, 32, true))),
        ("r8 C16 +Paeth", Some(c(true, 3, 16, true))),
    ];
    let names = ["portrait_0", "screen_0", "night_0"];
    let imgs: Vec<_> = names
        .iter()
        .map(|n| {
            let ext = if *n == "screen_0" { "png" } else { "jpg" };
            crop(Canonical::load_bytes(std::fs::read(format!("data/{n}.{ext}")).unwrap()).unwrap())
        })
        .collect();
    // Returns per-image (bytes, enc_s, dec_s, ok) and total.
    let run = |v: &Option<Cfg>| -> Vec<(usize, f64, f64, bool)> {
        let mut out = vec![];
        for img in &imgs {
            let (w, h) = (img.spec.width as usize, img.spec.height as usize);
            let mut rs = vec![];
            for y in [false, true] {
                let prod = pkon::encode_standard_with(img, if y { EllieRepr::YCoCgR } else { EllieRepr::Untransformed }).unwrap();
                let (planes, bits) = planes_of(img, y);
                let base = base_encode(&planes, &bits, w, h);
                let ovh = prod.len() - tot(&base);
                let streams = match v {
                    None => base,
                    Some(cf) => dvs_encode(*cf, &planes, &bits, w, h),
                };
                rs.push((y, ovh, streams, planes, bits));
            }
            let mut ok = true;
            if let Some(cf) = v {
                let (_, _, s2, p2, b2) = &rs[1];
                ok &= dvs_encode(*cf, p2, b2, w, h) == *s2; // deterministic
            }
            let size = |r: &(bool, usize, Vec<Vec<u8>>, Vec<Vec<u16>>, Vec<u32>)| r.1 + tot(&r.2);
            let idx = if size(&rs[1]) < size(&rs[0]) { 1 } else { 0 };
            let bytes = size(&rs[idx]);
            let (mut te, mut td) = (f64::MAX, f64::MAX);
            for _ in 0..reps {
                let t = Instant::now();
                for y in [false, true] {
                    let (p, b) = planes_of(img, y);
                    match v {
                        None => drop(std::hint::black_box(base_encode(&p, &b, w, h))),
                        Some(cf) => drop(std::hint::black_box(dvs_encode(*cf, &p, &b, w, h))),
                    }
                }
                te = te.min(t.elapsed().as_secs_f64());
                let r = &rs[idx];
                let t = Instant::now();
                let mut back = match v {
                    None => base_decode(&r.2, &r.4, w, h),
                    Some(cf) => dvs_decode(*cf, &r.2, &r.4, w, h),
                };
                td = td.min(t.elapsed().as_secs_f64());
                ok &= back == r.3;
                if r.0 {
                    assert!(ellie::ycocg_r_inverse_planes(&mut back[..3]));
                }
                let ch = back.len();
                let mut data = vec![0u8; w * h * ch];
                for (k, pl) in back.iter().enumerate() {
                    for (i, &x) in pl.iter().enumerate() {
                        data[i * ch + k] = x as u8;
                    }
                }
                ok &= data == img.data;
            }
            out.push((bytes, te, td, ok));
        }
        out
    };
    let mut results = vec![];
    for (l, v) in &variants {
        let r = run(v);
        let t: usize = r.iter().map(|x| x.0).sum();
        println!("{l}: total {t}");
        for (n, x) in names.iter().zip(&r) {
            println!("   {n} {} enc {:.3}s dec {:.3}s lossless/det {}", x.0, x.1, x.2, x.3);
        }
        results.push((l.to_string(), t));
    }
    // Winner among r8 C-sweep, then test without Paeth.
    let best = (3..6).min_by_key(|&i| results[i].1).unwrap();
    let bc = variants[best].1.unwrap();
    println!("WINNER {} ; now without Paeth", variants[best].0);
    let r = run(&Some(Cfg { paeth: false, ..bc }));
    let t: usize = r.iter().map(|x| x.0).sum();
    println!("winner noPaeth: total {t}");
    for (n, x) in names.iter().zip(&r) {
        println!("   {n} {} enc {:.3}s dec {:.3}s lossless/det {}", x.0, x.1, x.2, x.3);
    }
    let _ = dir;
}
