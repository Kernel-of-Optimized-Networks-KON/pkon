//! Production p=2 promotion, three-image canary: complete pkon file bytes, timings, round trip, determinism.
//!   bench_ellie_pow_prod <reps>
use nanoscope::canon::{Canonical, Spec};
use nanoscope::pkon::{self, EllieRepr};
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

fn main() {
    if cfg!(debug_assertions) {
        eprintln!("use --release");
        std::process::exit(2);
    }
    let reps: usize = std::env::args().nth(1).unwrap().parse().unwrap();
    const BASE: [usize; 3] = [487_415, 281_140, 1_931_320];
    println!("image bytes p1_bytes delta enc_s dec_s lossless deterministic");
    let mut total = 0usize;
    for (i, name) in ["portrait_0", "screen_0", "night_0"].into_iter().enumerate() {
        let ext = if name == "screen_0" { "png" } else { "jpg" };
        let img = crop(Canonical::load_bytes(std::fs::read(format!("data/{name}.{ext}")).unwrap()).unwrap());
        let f = pkon::encode_standard_with(&img, EllieRepr::Auto).unwrap();
        let (mut te, mut td, mut ok, mut det) = (f64::MAX, f64::MAX, true, true);
        for _ in 0..reps {
            let t = Instant::now();
            let g = pkon::encode_standard_with(&img, EllieRepr::Auto).unwrap();
            te = te.min(t.elapsed().as_secs_f64());
            det &= g == f;
            let t = Instant::now();
            let d = pkon::decode(&g).unwrap();
            td = td.min(t.elapsed().as_secs_f64());
            let pkon::Decoded::Standard(back) = d else { panic!() };
            ok &= back.data == img.data;
        }
        total += f.len();
        println!("{name} {} {} {} {te:.4} {td:.4} {ok} {det}", f.len(), BASE[i], f.len() as i64 - BASE[i] as i64);
    }
    let base: usize = BASE.iter().sum();
    println!("TOTAL {total} baseline {base} delta {} ({:.4}%)", total as i64 - base as i64, (total as f64 - base as f64) * 100.0 / base as f64);
}
