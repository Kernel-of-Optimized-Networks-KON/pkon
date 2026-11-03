//! Diagnostic micro-ablation of the Stage 1 predictor arithmetic. `predict` and `Shrub` are verbatim copies of the private
//! functions in src/ellie.rs (as of 10d67a8), driven by real neighbourhoods from the decoded planes. The `nodiv` variants replace the
//! divisions with shifts (WRONG results, timing bound only) to show how much of the predictor time the divisions account for.
//! usage: predict <file.pkon> [reps]
use nanoscope::lisa_frame::BayerFrame;
use nanoscope::pkon::{self, CodecParams, Decoded};
use std::time::Instant;

#[inline(always)]
fn paeth(a: i32, b: i32, c: i32) -> i32 {
    let p = a + b - c;
    let (pa, pb, pc) = ((p - a).abs(), (p - b).abs(), (p - c).abs());
    if pa <= pb && pa <= pc {
        a
    } else if pb <= pc {
        b
    } else {
        c
    }
}

#[inline(always)]
fn predict<const DIV: bool>(m: i32, w: i32, ww: i32, n: i32, nn: i32, nw: i32, ne: i32, s_h: u32, s_v: u32, s45: u32, s135: u32, fa: [u64; 3]) -> (i32, [u64; 3]) {
    let mn = s45.min(s135);
    let s_d = mn + (mn >> 2);
    let smax = s_h.max(s_v).max(s_d);
    let sh = (32 - (smax | 1).leading_zeros()).saturating_sub(10);
    let sq = |s: u32| {
        let r = ((s >> sh) + 1) as u64;
        r * r
    };
    let (qh, qv, qd) = (sq(s_h), sq(s_v), sq(s_d));
    let (ph, pv, pd) = ((qv * qd) as u128 * fa[0] as u128, (qh * qd) as u128 * fa[1] as u128, (qh * qv) as u128 * fa[2] as u128);
    let d = ph + pv + pd;
    let f = 2 + (s135 >= 2 * s45) as i32 - (s45 >= 2 * s135) as i32;
    let o = 4 * m;
    let h4 = (4 * w + ((w - ww + n - nw) >> 1) + o) as u64;
    let v4 = (4 * n + ((n - nn + w - nw) >> 1) + o) as u64;
    let d4 = (((3 * ((4 - f) * nw + f * ne)) / 4 + (w + ne - n) + o + 4 * paeth(w, n, nw) + o + 1) >> 1) as u64;
    let num = ph * h4 as u128 + pv * v4 as u128 + pd * d4 as u128;
    let t = if DIV { (num / d) as u64 } else { (num >> (d.leading_zeros().min(120) as u32 ^ 1) % 100) as u64 };
    ((((t + 2) >> 2) as i64 - m as i64).clamp(0, m as i64) as i32, [h4, v4, d4])
}

#[derive(Clone, Copy)]
struct Shrub {
    e: [i64; 3],
}
impl Shrub {
    #[inline(always)]
    fn factors<const DIV: bool>(&self) -> [u64; 3] {
        let mn = self.e[0].min(self.e[1]).min(self.e[2]);
        let f = |e: i64| {
            let r = if DIV { (((mn + 16) as u64) << 16) / (e + 16) as u64 } else { (((mn + 16) as u64) << 16) >> ((e as u64 >> 8) & 15) };
            ((256 * ((r * r) >> 16) + 32768) >> 16).max(1)
        };
        [f(self.e[0]), f(self.e[1]), f(self.e[2])]
    }
    #[inline(always)]
    fn update(&mut self, x: i32, est4: [u64; 3], m: i32) {
        let t = 4 * x as i64 + 4 * m as i64;
        for k in 0..3 {
            let err = (t - est4[k] as i64).abs() << 4;
            self.e[k] += (err - self.e[k]) >> 3;
        }
    }
}

fn run<const DIV_P: bool, const DIV_F: bool>(pl: &[u16], w: usize, h: usize, m: i32) -> u64 {
    let mut shrub = Shrub { e: [0; 3] };
    let mut acc = 0u64;
    let g = |x: usize, y: usize| pl[y * w + x] as i32;
    for y in 2..h {
        for x in 2..w - 2 {
            let (wv, wwv, n, nw, ne, nn) = (g(x - 1, y), g(x - 2, y), g(x, y - 1), g(x - 1, y - 1), g(x + 1, y - 1), g(x, y - 2));
            let s_h = ((wv - wwv).abs() + (n - nw).abs() + (ne - n).abs()) as u32;
            let s_v = ((n - nn).abs() + (wv - nw).abs() + (ne - g(x + 1, y - 2)).abs()) as u32;
            let s45 = ((wv - nw).abs() + (n - ne).abs()) as u32;
            let s135 = ((nw - g(x - 2, y - 1)).abs() + (n - wv).abs()) as u32;
            let (pr, est) = predict::<DIV_P>(m, wv, wwv, n, nn, nw, ne, s_h, s_v, s45, s135, shrub.factors::<DIV_F>());
            let xv = g(x, y);
            shrub.update(xv, est, m);
            acc = acc.wrapping_add(pr as u64);
        }
    }
    acc
}

fn best<R>(reps: usize, mut f: impl FnMut() -> R) -> f64 {
    (0..reps)
        .map(|_| {
            let s = Instant::now();
            std::hint::black_box(f());
            s.elapsed().as_secs_f64()
        })
        .fold(f64::MAX, f64::min)
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let bytes = std::fs::read(&a[1]).unwrap();
    let reps: usize = a.get(2).and_then(|s| s.parse().ok()).unwrap_or(3);
    let hdr = pkon::parse_header(&bytes).unwrap();
    let CodecParams::Ulisa(p) = &hdr.params else { panic!("not codec 7") };
    let frame = BayerFrame::with_lattice(p.metadata.clone(), hdr.bit_depth, p.lattice).unwrap();
    let Decoded::Raw(img) = pkon::decode(&bytes).unwrap() else { panic!() };
    let planes = frame.split(&img.samples);
    let (w, h) = frame.plane_dims()[0];
    let m = (1i32 << frame.coding_depth()) - 1;
    let n = ((w - 4) * (h - 2)) as f64;
    let t0 = best(reps, || run::<true, true>(&planes[0], w, h, m));
    let t1 = best(reps, || run::<false, true>(&planes[0], w, h, m));
    let t2 = best(reps, || run::<true, false>(&planes[0], w, h, m));
    let t3 = best(reps, || run::<false, false>(&planes[0], w, h, m));
    println!("plane 0 {w}x{h}: predict+Shrub over real neighbourhoods, ns/sample (best of {reps})");
    println!("  verbatim (u128 div + 3 u64 div) {:6.1}", t0 / n * 1e9);
    println!("  no u128 division               {:6.1}", t1 / n * 1e9);
    println!("  no Shrub u64 divisions         {:6.1}", t2 / n * 1e9);
    println!("  no divisions at all            {:6.1}", t3 / n * 1e9);
}
