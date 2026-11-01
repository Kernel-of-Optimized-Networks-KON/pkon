//! Adaptive-rate Shrub vs fixed 1/8 (both Paeth, C=16), three-image canary, complete-file bytes, round trip through the experimental pipeline.
//!   bench_ellie_dvs_adaptive <reps>
#[path = "s1a.rs"]
mod s1a;
use nanoscope::canon::{Canonical, Spec};
use nanoscope::ellie::{self, Stage2, Stage3, Stage3Profile, Stage4Dec, Stage4Enc};
use nanoscope::pkon::{self, EllieRepr};
use s1a::{Cfg, Stage1A};
use std::time::Instant;

fn crop(c: Canonical) -> Canonical {
    let Spec { width, height, layout, bit_depth } = c.spec;
    let (nw, nh) = (width.min(1024), height.min(1024));
    if (nw, nh) == (width, height) {
        return c;
    }
    let (x0, y0) = ((width - nw) as usize / 2, (height - nh) as usize / 2);
    let ch = layout.channels();
    let mut data = Vec::new();
    for y in 0..nh as usize {
        let s = ((y0 + y) * width as usize + x0) * ch;
        data.extend_from_slice(&c.data[s..s + nw as usize * ch]);
    }
    let mut out = Canonical::new(Spec { width: nw, height: nh, layout, bit_depth }, data).unwrap();
    out.notes = c.notes;
    out
}

fn planes_of(img: &Canonical, ycocg: bool) -> (Vec<Vec<u16>>, Vec<u32>) {
    let (w, h) = (img.spec.width as usize, img.spec.height as usize);
    let ch = img.spec.layout.channels();
    let mut planes = vec![vec![0u16; w * h]; ch];
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

/// Encode each plane to its own stream; returns the streams.
fn enc_streams(w: usize, h: usize, planes: &[Vec<u16>], bits: &[u32], cfg: Cfg) -> Vec<Vec<u8>> {
    let mut enc = Stage4Enc::with_capacity(w * h);
    let mut res = vec![0i32; w * h];
    let mut out = Vec::new();
    for (p, &b) in planes.iter().zip(bits) {
        let mut s1 = Stage1A::new(w, h, b, cfg).unwrap();
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

fn dec_streams(w: usize, h: usize, streams: &[Vec<u8>], bits: &[u32], cfg: Cfg) -> Vec<Vec<u16>> {
    let mut res = vec![0i32; w * h];
    let mut out = Vec::new();
    for (s, &b) in streams.iter().zip(bits) {
        let mut s1 = Stage1A::new(w, h, b, cfg).unwrap();
        let mut s3 = Stage3::with_profile(w, h, b, Stage3Profile::Bc255S11).unwrap();
        s3.reset();
        let mut dec = Stage4Dec::new(s);
        s3.decode_plane(&mut res, &mut dec).unwrap();
        Stage2::new().decode_in_place(&mut res);
        let mut p = vec![0u16; w * h];
        s1.scan(|i, pr| {
            let v = (pr as i32 + res[i]) as u16;
            p[i] = v;
            v as u32
        });
        out.push(p);
    }
    out
}

/// Container overhead (header + metadata) for a representation: production file length minus its plane streams.
fn overhead(img: &Canonical, prod_file: &[u8], ycocg: bool) -> usize {
    let (w, h) = (img.spec.width as usize, img.spec.height as usize);
    let (planes, bits) = planes_of(img, ycocg);
    let mut enc = Stage4Enc::with_capacity(w * h);
    let mut s = 0;
    for (p, &b) in planes.iter().zip(&bits) {
        let mut c = ellie::EllieCodec::with_profile(w, h, b, Stage3Profile::Bc255S11).unwrap();
        enc.reset();
        c.encode_planes(&[p.as_slice()], &mut enc).unwrap();
        s += enc.finish().len();
    }
    prod_file.len() - s
}

fn main() {
    if cfg!(debug_assertions) {
        eprintln!("use --release");
        std::process::exit(2);
    }
    let reps: usize = std::env::args().nth(1).unwrap().parse().unwrap();
    let fixed = Cfg { shrub: true, shift: 3, c: 16, paeth: true, adaptive: false };
    let adapt = Cfg { adaptive: true, ..fixed };
    println!("image prod_B fixed_B adapt_B delta% | enc_fixed enc_adapt dec_fixed dec_adapt (s) | lossless det");
    let (mut tp, mut tf, mut ta) = (0usize, 0usize, 0usize);
    for name in ["portrait_0", "screen_0", "night_0"] {
        let ext = if name == "screen_0" { "png" } else { "jpg" };
        let img = crop(Canonical::load_bytes(std::fs::read(format!("data/{name}.{ext}")).unwrap()).unwrap());
        let (w, h) = (img.spec.width as usize, img.spec.height as usize);
        let prod = pkon::encode_standard_with(&img, EllieRepr::Auto).unwrap();
        // Per config: best repr by complete-file bytes; keep its streams for timing/round trip.
        let mut res = Vec::new();
        for cfg in [fixed, adapt] {
            let mut best: Option<(usize, bool, Vec<Vec<u8>>)> = None;
            for (y, r) in [(false, EllieRepr::Untransformed), (true, EllieRepr::YCoCgR)] {
                let pf = pkon::encode_standard_with(&img, r).unwrap();
                let (planes, bits) = planes_of(&img, y);
                let st = enc_streams(w, h, &planes, &bits, cfg);
                let total = st.iter().map(|s| s.len()).sum::<usize>() + overhead(&img, &pf, y);
                if best.as_ref().map_or(true, |b| total < b.0) {
                    best = Some((total, y, st));
                }
            }
            res.push(best.unwrap());
        }
        let mut times = [[f64::MAX; 2]; 2]; // [cfg][enc,dec]
        let mut ok = true;
        let mut det = true;
        for (ci, cfg) in [fixed, adapt].into_iter().enumerate() {
            let y = res[ci].1;
            let (planes, bits) = planes_of(&img, y);
            for _ in 0..reps {
                let t = Instant::now();
                let st = enc_streams(w, h, &planes, &bits, cfg);
                times[ci][0] = times[ci][0].min(t.elapsed().as_secs_f64());
                det &= st == res[ci].2;
                let t = Instant::now();
                let mut back = dec_streams(w, h, &st, &bits, cfg);
                times[ci][1] = times[ci][1].min(t.elapsed().as_secs_f64());
                ok &= back == planes;
                if y {
                    assert!(ellie::ycocg_r_inverse_planes(&mut back[..3]));
                }
                let ch = img.spec.layout.channels();
                for (i, px) in img.data.chunks_exact(ch).enumerate() {
                    for (k, &v) in px.iter().enumerate() {
                        ok &= back[k][i] == v as u16;
                    }
                }
            }
        }
        let (bf, ba) = (res[0].0, res[1].0);
        tp += prod.len();
        tf += bf;
        ta += ba;
        println!(
            "{name} {} {bf} {ba} {:+.3}% | {:.3} {:.3} {:.3} {:.3} | {ok} {det}",
            prod.len(),
            (ba as f64 - bf as f64) * 100.0 / bf as f64,
            times[0][0], times[1][0], times[0][1], times[1][1]
        );
    }
    println!("TOTAL prod {tp} fixed {tf} adaptive {ta} delta {} ({:+.3}%)", ta as i64 - tf as i64, (ta as f64 - tf as f64) * 100.0 / tf as f64);
}
