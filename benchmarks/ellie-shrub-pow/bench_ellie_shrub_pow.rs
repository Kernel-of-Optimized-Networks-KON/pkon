//! Shrub relative-weighting exponent experiment (isolated): p in {1, 1.25, 1.5, 2}, three-image canary, 1024 centre crops.
//!   bench_ellie_shrub_pow <reps>
#[path = "s1p.rs"]
mod s1p;
use nanoscope::canon::{Canonical, Spec};
use nanoscope::ellie::{self, Stage2, Stage3, Stage3Profile, Stage4Dec, Stage4Enc};
use nanoscope::pkon::{self, EllieRepr};
use s1p::{Cfg, Stage1D};
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

/// Encode all planes: returns the concatenated Stage 4 streams (length-prefixed per plane for decoding here).
fn encode(planes: &[Vec<u16>], bits: &[u32], w: usize, h: usize, cfg: Cfg) -> Vec<Vec<u8>> {
    let mut enc = Stage4Enc::with_capacity(w * h);
    let mut res = vec![0i32; w * h];
    let mut out = Vec::new();
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

fn decode(streams: &[Vec<u8>], bits: &[u32], w: usize, h: usize, cfg: Cfg) -> Vec<Vec<u16>> {
    let mut res = vec![0i32; w * h];
    let mut planes = Vec::new();
    for (st, &b) in streams.iter().zip(bits) {
        let mut s1 = Stage1D::new(w, h, b, cfg).unwrap();
        let mut s3 = Stage3::with_profile(w, h, b, Stage3Profile::Bc255S11).unwrap();
        s3.reset();
        let mut dec = Stage4Dec::new(st);
        s3.decode_plane(&mut res, &mut dec).unwrap();
        Stage2::new().decode_in_place(&mut res);
        let mut out = vec![0u16; w * h];
        s1.scan(|i, pr| {
            let v = (pr as i32 + res[i]) as u32;
            out[i] = v as u16;
            v
        });
        planes.push(out);
    }
    planes
}

/// Container overhead = production file length minus production plane streams.
fn overhead(img: &Canonical, prod_len: usize, ycocg: bool) -> usize {
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
    prod_len - s
}

fn main() {
    if cfg!(debug_assertions) {
        eprintln!("use --release");
        std::process::exit(2);
    }
    let reps: usize = std::env::args().nth(1).unwrap().parse().unwrap();
    let names = ["portrait_0", "screen_0", "night_0"];
    let imgs: Vec<_> = names
        .iter()
        .map(|n| {
            let ext = if *n == "screen_0" { "png" } else { "jpg" };
            let img = crop(Canonical::load_bytes(std::fs::read(format!("data/{n}.{ext}")).unwrap()).unwrap());
            let ov: Vec<usize> = [(false, EllieRepr::Untransformed), (true, EllieRepr::YCoCgR)]
                .iter()
                .map(|&(y, r)| overhead(&img, pkon::encode_standard_with(&img, r).unwrap().len(), y))
                .collect();
            (img, ov)
        })
        .collect();
    let mut base = [0usize; 3];
    println!("p image bytes delta% enc_s dec_s repr lossless deterministic");
    for (label, pq) in [("1", 4), ("1.25", 5), ("1.5", 6), ("2", 8)] {
        let cfg = Cfg { shrub: true, shift: 3, c: 16, paeth: true, pq };
        let mut total = 0usize;
        let (mut tot_e, mut tot_d) = (0.0, 0.0);
        let mut all_ok = true;
        for (k, (img, ov)) in imgs.iter().enumerate() {
            let (w, h) = (img.spec.width as usize, img.spec.height as usize);
            let mut best = (usize::MAX, false);
            for (ri, y) in [false, true].into_iter().enumerate() {
                let (pl, bits) = planes_of(img, y);
                let st = encode(&pl, &bits, w, h, cfg);
                let n = st.iter().map(|s| s.len()).sum::<usize>() + ov[ri];
                if n < best.0 {
                    best = (n, y);
                }
            }
            let (pl, bits) = planes_of(img, best.1);
            let (mut te, mut td) = (f64::MAX, f64::MAX);
            let (mut ok, mut det) = (true, true);
            let first = encode(&pl, &bits, w, h, cfg);
            for _ in 0..reps {
                let t = Instant::now();
                let st = encode(&pl, &bits, w, h, cfg);
                te = te.min(t.elapsed().as_secs_f64());
                det &= st == first;
                let t = Instant::now();
                let back = decode(&st, &bits, w, h, cfg);
                td = td.min(t.elapsed().as_secs_f64());
                ok &= back == pl;
            }
            if pq == 4 {
                base[k] = best.0;
            }
            total += best.0;
            tot_e += te;
            tot_d += td;
            all_ok &= ok && det;
            println!("{label} {} {} {:+.3} {te:.4} {td:.4} {} {ok} {det}", names[k], best.0, (best.0 as f64 / base[k] as f64 - 1.0) * 100.0, if best.1 { "YCoCg-R" } else { "RGB" });
        }
        let bt: usize = base.iter().sum();
        println!("{label} TOTAL {total} {:+.3} {tot_e:.4} {tot_d:.4} all_ok={all_ok}", (total as f64 / bt as f64 - 1.0) * 100.0);
    }
}
