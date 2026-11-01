//! Production Shrub validation, three-image canary: production pkon file bytes vs the experimental winner (1/8, C=16, Paeth).
//!   bench_ellie_shrub_prod <reps>
#[path = "../ellie-deja-vu-shrub/s1d.rs"]
mod s1d;
use nanoscope::canon::{Canonical, Spec};
use nanoscope::ellie::{self, Stage2, Stage3, Stage3Profile, Stage4Enc};
use nanoscope::pkon::{self, EllieRepr};
use s1d::{Cfg, Stage1D};
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

/// Experimental winner stream bytes for one representation (plane streams only).
fn exp_streams(img: &Canonical, ycocg: bool) -> usize {
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
    let cfg = Cfg { shrub: true, shift: 3, c: 16, paeth: true };
    let mut enc = Stage4Enc::with_capacity(w * h);
    let mut res = vec![0i32; w * h];
    let mut total = 0;
    for (p, &b) in planes.iter().zip(&bits) {
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
        total += enc.finish().len();
    }
    total
}

fn main() {
    if cfg!(debug_assertions) {
        eprintln!("use --release");
        std::process::exit(2);
    }
    let reps: usize = std::env::args().nth(1).unwrap().parse().unwrap();
    println!("image prod_B exp_B match enc_s dec_s lossless deterministic");
    let (mut tp, mut te) = (0usize, 0usize);
    for name in ["portrait_0", "screen_0", "night_0"] {
        let ext = if name == "screen_0" { "png" } else { "jpg" };
        let img = crop(Canonical::load_bytes(std::fs::read(format!("data/{name}.{ext}")).unwrap()).unwrap());
        let f = pkon::encode_standard_with(&img, EllieRepr::Auto).unwrap();
        let det = f == pkon::encode_standard_with(&img, EllieRepr::Auto).unwrap();
        // Experimental complete-file size = container overhead (from production, per repr) + experimental streams.
        let mut best = usize::MAX;
        for (y, r) in [(false, EllieRepr::Untransformed), (true, EllieRepr::YCoCgR)] {
            let pf = pkon::encode_standard_with(&img, r).unwrap();
            // overhead = production file minus production plane streams; obtained from the old baseline relation below.
            best = best.min(exp_streams(&img, y) + overhead(&img, &pf, y));
        }
        let (mut te_s, mut td_s, mut ok) = (f64::MAX, f64::MAX, true);
        for _ in 0..reps {
            let t = Instant::now();
            let g = pkon::encode_standard_with(&img, EllieRepr::Auto).unwrap();
            te_s = te_s.min(t.elapsed().as_secs_f64());
            let t = Instant::now();
            let d = pkon::decode(&g).unwrap();
            td_s = td_s.min(t.elapsed().as_secs_f64());
            let pkon::Decoded::Standard(back) = d else { panic!() };
            ok &= back.data == img.data;
        }
        tp += f.len();
        te += best;
        println!("{name} {} {best} {} {te_s:.4} {td_s:.4} {ok} {det}", f.len(), f.len() == best);
    }
    println!("TOTAL prod {tp} exp {te} baseline 2726990 delta {} ({:.3}%)", tp as i64 - 2726990, (tp as f64 - 2726990.0) * 100.0 / 2726990.0);
}

/// Container overhead (header + metadata) for a representation: production file length minus its plane streams.
fn overhead(img: &Canonical, prod_file: &[u8], ycocg: bool) -> usize {
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
