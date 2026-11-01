//! Axis C probe (experiment-only): where does buffering / windowing time go in the production pipeline?
//! Measures, on the Y plane (8-bit) and a Co plane (9-bit) of the 3-image set, release mode, best of N:
//!   * Stage 1 + Stage 2 (plane -> residual buffer)           * Stage 3 + 4 encode (residual buffer -> bytes)
//!   * EllieCodec::new (allocation + zeroing)                 * one extra streaming pass over the i32 residual buffer
//!     (an upper bound on what fusing Stage 1/2/3 into cache-sized bands could save: the res buffer is written once and read twice)
use nanoscope::canon::Canonical;
use nanoscope::ellie::{self, EllieCodec, Stage1, Stage2, Stage4Enc};
use std::time::Instant;

fn best<F: FnMut()>(n: usize, mut f: F) -> f64 {
    (0..n).map(|_| { let t = Instant::now(); f(); t.elapsed().as_secs_f64() }).fold(f64::MAX, f64::min)
}

fn main() {
    let data = std::path::Path::new("data");
    for f in ["portrait_0.jpg", "screen_0.png", "night_0.jpg"] {
        let img = Canonical::load(&data.join(f)).unwrap();
        let (w0, h0) = (img.spec.width as usize, img.spec.height as usize);
        let ch = img.spec.layout.channels();
        let (w, h) = (w0.min(1024), h0.min(1024));
        let (x0, y0) = ((w0 - w) / 2, (h0 - h) / 2);
        let mut planes = vec![vec![0u16; w * h]; ch];
        for y in 0..h {
            for x in 0..w {
                for k in 0..ch {
                    planes[k][y * w + x] = img.data[((y0 + y) * w0 + x0 + x) * ch + k] as u16;
                }
            }
        }
        assert!(ellie::ycocg_r_forward_planes(&mut planes[..3]));
        for (name, k, bits) in [("Y", 0usize, 8u32), ("Co", 1, 9)] {
            let n = w * h;
            let mut res = vec![0i32; n];
            let mut s1 = Stage1::new(w, h, bits).unwrap();
            let t12 = best(5, || {
                s1.encode(&planes[k], &mut res).unwrap();
                Stage2::new().encode_in_place(&mut res);
            });
            let mut codec = EllieCodec::new(w, h, bits).unwrap();
            let mut enc = Stage4Enc::with_capacity(n);
            let tfull = best(5, || {
                enc.reset();
                codec.encode_planes(&[planes[k].as_slice()], &mut enc).unwrap();
                enc.finish();
            });
            let talloc = best(20, || {
                std::hint::black_box(EllieCodec::new(w, h, bits).unwrap());
            });
            let mut sink = 0i64;
            let tpass = best(20, || {
                sink = sink.wrapping_add(res.iter().map(|&v| v as i64).sum::<i64>());
            });
            std::hint::black_box(sink);
            println!(
                "{f} {name}: stage1+2 {:.1} ms | full encode_planes (incl. stage1/2/3/4) {:.1} ms -> stage3+4 ~{:.1} ms | EllieCodec::new {:.2} ms | one res pass {:.2} ms ({:.2}% of encode) | res buffer {} KiB",
                t12 * 1e3, tfull * 1e3, (tfull - t12) * 1e3, talloc * 1e3, tpass * 1e3, tpass / tfull * 100.0, n * 4 / 1024
            );
        }
    }
}
