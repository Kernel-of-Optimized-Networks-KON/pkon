//! ELLIE Stage 2 experiment: causal residual lifting step. Diagnostic only; no production source is modified.
//!
//! Hypothesis: Stage 1 residuals e keep signed spatial correlation (lag-1 about 0.12-0.26). A lifting step predicts e_i from
//! already-decoded neighbouring residuals with fixed integer weights and codes r_i = e_i - P_i. The decoder holds e for every
//! earlier sample, so e_i = r_i + P_i is exact. No state, no metadata. Stage 1, 3, 4 and the container are production code.
//!
//!   cargo run --release --bin exp_ellie_stage2_lift
use nanoscope::canon::{Canonical, Spec};
use nanoscope::ellie::{self, Stage1, Stage2, Stage3, Stage3Profile, Stage4Dec, Stage4Enc};
use nanoscope::pkon::{self, CodecParams, EllieRepr, ELLIE_REPR_YCOCG_R, FIXED_HEADER_LEN};
use nanoscope::rangecoder::BitIo;
use std::time::Instant;

const PROFILE: Stage3Profile = Stage3Profile::Bc255S11;
const SH: u32 = 5; // weights are in 1/32

fn center_crop(img: &Canonical, max: u32) -> Canonical {
    let (w, h) = (img.spec.width, img.spec.height);
    if w <= max && h <= max {
        return img.clone();
    }
    let (cw, ch) = (w.min(max), h.min(max));
    let (x0, y0) = ((w - cw) / 2, (h - ch) / 2);
    let row = img.spec.layout.channels() * img.spec.bytes_per_sample();
    let mut data = Vec::new();
    for y in y0..y0 + ch {
        let s = (y as usize * w as usize + x0 as usize) * row;
        data.extend_from_slice(&img.data[s..s + cw as usize * row]);
    }
    Canonical::new(Spec { width: cw, height: ch, ..img.spec }, data).unwrap()
}
fn split_planes(img: &Canonical, ycocg: bool) -> Vec<Vec<u16>> {
    let ch = img.spec.layout.channels();
    let n = img.spec.width as usize * img.spec.height as usize;
    let mut planes = vec![vec![0u16; n]; ch];
    for (i, px) in img.data.chunks_exact(ch).enumerate() {
        for (k, &v) in px.iter().enumerate() {
            planes[k][i] = v as u16;
        }
    }
    if ycocg {
        assert!(ellie::ycocg_r_forward_planes(&mut planes[..3]));
    }
    planes
}

// ---------------------------------------------------------------- the candidate transform (exact integer lifting step)
/// Weights on (W, N, NW, NE) in 1/32 and a rounding offset: P = sign(s) * ((|s| + off) >> SH), s = sum w*neighbour.
/// off = 16 is round-to-nearest; smaller values widen the dead zone around 0 so P stays 0 for small neighbourhoods.
#[derive(Clone, Copy, Debug)]
struct Lift {
    w: [i32; 4],
    off: i32,
}
#[inline(always)]
fn pred(l: &Lift, e: &[i32], w: usize, x: usize, y: usize, i: usize, m: i32) -> i32 {
    let g = |ok: bool, j: usize| if ok { e[j] } else { 0 };
    let nb = [
        g(x > 0, i.wrapping_sub(1)),
        g(y > 0, i.wrapping_sub(w)),
        g(x > 0 && y > 0, i.wrapping_sub(w + 1)),
        g(y > 0 && x + 1 < w, i.wrapping_sub(w - 1)),
    ];
    let s: i32 = (0..4).map(|k| l.w[k] * nb[k]).sum();
    let p = if s >= 0 { (s + l.off) >> SH } else { -((-s + l.off) >> SH) };
    p.clamp(-m, m)
}
fn lift_fwd(l: &Lift, e: &[i32], w: usize, h: usize, m: i32) -> Vec<i32> {
    let mut r = vec![0i32; w * h];
    for y in 0..h {
        for x in 0..w {
            let i = y * w + x;
            r[i] = e[i] - pred(l, e, w, x, y, i, m);
        }
    }
    r
}
/// Inverse: sequential; the predictor reads only already-reconstructed samples.
fn lift_inv(l: &Lift, r: &[i32], w: usize, h: usize, m: i32) -> Vec<i32> {
    let mut e = vec![0i32; w * h];
    for y in 0..h {
        for x in 0..w {
            let i = y * w + x;
            e[i] = r[i] + pred(l, &e, w, x, y, i, m);
        }
    }
    e
}

// ---------------------------------------------------------------- measurement
struct CostIo {
    tab: Vec<f64>,
    cost: f64,
}
impl BitIo for CostIo {
    #[inline(always)]
    fn code(&mut self, p1: u32, bit: u32) -> u32 {
        self.cost += self.tab[if bit != 0 { p1 } else { 65536 - p1 } as usize];
        bit
    }
}
fn s3_cost(r: &[i32], w: usize, h: usize, bits: u32) -> f64 {
    let mut s3 = Stage3::with_profile(w, h, bits, PROFILE).unwrap();
    let mut io = CostIo { tab: (0..=65536usize).map(|p| if p == 0 { f64::INFINITY } else { -(p as f64 / 65536.0).log2() }).collect(), cost: 0.0 };
    s3.encode_plane(r, &mut io).unwrap();
    io.cost
}

/// Least-squares normal equations for e ~ W,N,NW,NE (analysis only, f64; production uses the quantised integers).
#[derive(Clone, Copy, Default)]
struct Ne {
    a: [[f64; 4]; 4],
    b: [f64; 4],
}
fn accum(ne: &mut Ne, e: &[i32], w: usize, h: usize) {
    for y in 1..h {
        for x in 1..w - 1 {
            let i = y * w + x;
            let nb = [e[i - 1] as f64, e[i - w] as f64, e[i - w - 1] as f64, e[i - w + 1] as f64];
            for p in 0..4 {
                ne.b[p] += nb[p] * e[i] as f64;
                for q in 0..4 {
                    ne.a[p][q] += nb[p] * nb[q];
                }
            }
        }
    }
}
fn add(a: &Ne, b: &Ne) -> Ne {
    let mut o = *a;
    for p in 0..4 {
        o.b[p] += b.b[p];
        for q in 0..4 {
            o.a[p][q] += b.a[p][q];
        }
    }
    o
}
fn solve(ne: &Ne) -> [f64; 4] {
    let mut m = [[0f64; 5]; 4];
    for p in 0..4 {
        m[p][..4].copy_from_slice(&ne.a[p]);
        m[p][4] = ne.b[p];
    }
    for c in 0..4 {
        let piv = (c..4).max_by(|&i, &j| m[i][c].abs().total_cmp(&m[j][c].abs())).unwrap();
        m.swap(c, piv);
        for r in 0..4 {
            if r != c {
                let f = m[r][c] / m[c][c];
                for k in c..5 {
                    m[r][k] -= f * m[c][k];
                }
            }
        }
    }
    [m[0][4] / m[0][0], m[1][4] / m[1][1], m[2][4] / m[2][2], m[3][4] / m[3][3]]
}
fn quant(w: [f64; 4]) -> [i32; 4] {
    w.map(|v| (v * 32.0).round() as i32)
}

struct Img {
    name: String,
    w: usize,
    h: usize,
    bits: Vec<u32>,
    file_len: usize,
    payload_len: usize,
    planes: Vec<Vec<u16>>,
    e: Vec<Vec<i32>>,
}

fn main() {
    if cfg!(debug_assertions) {
        eprintln!("use --release");
        std::process::exit(2);
    }
    let data = std::path::PathBuf::from("data");
    let mut imgs: Vec<Img> = vec![];
    for f in ["portrait_0.jpg", "screen_0.png", "night_0.jpg"] {
        let img = center_crop(&Canonical::load(&data.join(f)).unwrap(), 1024);
        let (w, h) = (img.spec.width as usize, img.spec.height as usize);
        let prod = pkon::encode_standard_with(&img, EllieRepr::Auto).unwrap();
        let hdr = pkon::parse_header(&prod).unwrap();
        let CodecParams::Ellie(p) = &hdr.params else { panic!("not ELLIE") };
        let ycocg = p.representation == ELLIE_REPR_YCOCG_R;
        let bits: Vec<u32> = p.planes.iter().map(|x| x.0 as u32).collect();
        let payload_len = prod.len() - FIXED_HEADER_LEN - hdr.meta_len as usize;
        let planes = split_planes(&img, ycocg);
        let mut s1 = [Stage1::new(w, h, 8).unwrap(), Stage1::new(w, h, 9).unwrap()];
        let e: Vec<Vec<i32>> = planes
            .iter()
            .zip(&bits)
            .map(|(pl, &b)| {
                let mut e = vec![0i32; w * h];
                s1[(b == 9) as usize].encode(pl, &mut e).unwrap();
                e
            })
            .collect();
        imgs.push(Img { name: f.split('.').next().unwrap().into(), w, h, bits, file_len: prod.len(), payload_len, planes, e });
    }

    // 1. structure discovery: least-squares causal weights, global and leave-one-image-out
    let nes: Vec<Ne> = imgs
        .iter()
        .map(|im| {
            let mut ne = Ne::default();
            for e in &im.e {
                accum(&mut ne, e, im.w, im.h);
            }
            ne
        })
        .collect();
    let all = nes.iter().fold(Ne::default(), |a, b| add(&a, b));
    let wg = solve(&all);
    println!("LS weights (W,N,NW,NE) global: {:.4?}  -> /32: {:?}", wg, quant(wg));
    let mut loo: Vec<[i32; 4]> = vec![];
    for (k, im) in imgs.iter().enumerate() {
        let mut ne = Ne::default();
        for (j, n) in nes.iter().enumerate() {
            if j != k {
                ne = add(&ne, n);
            }
        }
        let wl = solve(&ne);
        println!("  LS excluding {:<10} {:.4?} -> {:?}", im.name, wl, quant(wl));
        loo.push(quant(wl));
    }

    // 2. baseline = production Stage 2
    let mut base_cost = vec![0f64; imgs.len()];
    let mut base_t = (0f64, 0f64);
    for (k, im) in imgs.iter().enumerate() {
        for (pi, e) in im.e.iter().enumerate() {
            let t = Instant::now();
            let mut r = e.clone();
            Stage2::new().encode_in_place(&mut r);
            base_t.0 += t.elapsed().as_secs_f64();
            base_cost[k] += s3_cost(&r, im.w, im.h, im.bits[pi]);
            let t = Instant::now();
            Stage2::new().decode_in_place(&mut r);
            base_t.1 += t.elapsed().as_secs_f64();
            assert_eq!(&r, e);
        }
    }
    let bc: f64 = base_cost.iter().sum();
    let bf: usize = imgs.iter().map(|i| i.file_len).sum();
    let bp: usize = imgs.iter().map(|i| i.payload_len).sum();
    println!("\nBASELINE production Stage 2: total .pkon {bf} B (payload {bp} B), Stage3 model {bc:.0} bits; Stage 2 enc {:.1} ms dec {:.1} ms", base_t.0 * 1e3, base_t.1 * 1e3);
    for (k, im) in imgs.iter().enumerate() {
        println!("  {:<10} file {} B payload {} B model {:.0} bits", im.name, im.file_len, im.payload_len, base_cost[k]);
    }

    // 3. variants (None = identity, i.e. Stage 2 bypassed)
    let g = quant(wg);
    let n = imgs.len();
    let mut runs: Vec<(String, Vec<Option<Lift>>)> = vec![("identity (Stage 2 bypassed)".into(), vec![None; n])];
    for off in [16, 12, 8] {
        runs.push((format!("lift LS-global {g:?}/32 off={off}"), vec![Some(Lift { w: g, off }); n]));
    }
    runs.push(("lift W+N only (4,4,0,0)/32 off=16".into(), vec![Some(Lift { w: [4, 4, 0, 0], off: 16 }); n]));
    for off in [16, 12] {
        runs.push((format!("lift LS leave-one-out off={off}"), loo.iter().map(|&w| Some(Lift { w, off })).collect()));
    }

    for (label, lifts) in &runs {
        let (mut tot_file, mut tot_cost) = (0usize, 0f64);
        let mut per = String::new();
        let (mut te, mut td) = (0f64, 0f64);
        let (mut zero_e, mut zero_r, mut cnt) = (0u64, 0u64, 0u64);
        for (k, im) in imgs.iter().enumerate() {
            let mut payload: Vec<Vec<u8>> = vec![];
            let mut cost = 0f64;
            let mut enc = Stage4Enc::with_capacity(im.w * im.h);
            let mut rs: Vec<Vec<i32>> = vec![];
            for (pi, e) in im.e.iter().enumerate() {
                let mp = (1i32 << im.bits[pi]) - 1;
                let t = Instant::now();
                let r = match lifts[k] {
                    Some(l) => lift_fwd(&l, e, im.w, im.h, mp),
                    None => e.clone(),
                };
                te += t.elapsed().as_secs_f64();
                cost += s3_cost(&r, im.w, im.h, im.bits[pi]);
                zero_e += e.iter().filter(|&&v| v == 0).count() as u64;
                zero_r += r.iter().filter(|&&v| v == 0).count() as u64;
                cnt += r.len() as u64;
                let mut s3 = Stage3::with_profile(im.w, im.h, im.bits[pi], PROFILE).unwrap();
                enc.reset();
                s3.encode_plane(&r, &mut enc).unwrap();
                payload.push(enc.finish().to_vec());
                rs.push(r);
            }
            // full decode chain: Stage 4 -> Stage 3 -> inverse lift -> Stage 1, compared with the original planes
            let mut s1 = [Stage1::new(im.w, im.h, 8).unwrap(), Stage1::new(im.w, im.h, 9).unwrap()];
            for (pi, bytes) in payload.iter().enumerate() {
                let mut s3 = Stage3::with_profile(im.w, im.h, im.bits[pi], PROFILE).unwrap();
                let mut dec = Stage4Dec::new(bytes);
                let mut r = vec![0i32; im.w * im.h];
                s3.decode_plane(&mut r, &mut dec).unwrap();
                let mp = (1i32 << im.bits[pi]) - 1;
                let t = Instant::now();
                let e = match lifts[k] {
                    Some(l) => lift_inv(&l, &r, im.w, im.h, mp),
                    None => r,
                };
                td += t.elapsed().as_secs_f64();
                assert_eq!(e, im.e[pi], "e mismatch: {label} {} plane {pi}", im.name);
                let mut out = vec![0u16; im.w * im.h];
                s1[(im.bits[pi] == 9) as usize].decode(&e, &mut out).unwrap();
                assert_eq!(out, im.planes[pi], "pixel mismatch: {label} {} plane {pi}", im.name);
                if let Some(l) = lifts[k] {
                    assert_eq!(lift_fwd(&l, &im.e[pi], im.w, im.h, mp), rs[pi], "non-deterministic forward");
                }
            }
            let pl: usize = payload.iter().map(|p| p.len()).sum();
            let file = im.file_len - im.payload_len + pl;
            tot_file += file;
            tot_cost += cost;
            per.push_str(&format!(
                "    {:<10} file {} B ({:+.2}%)  model {:.0} bits ({:+.2}%)\n",
                im.name,
                file,
                100.0 * (file as f64 / im.file_len as f64 - 1.0),
                cost,
                100.0 * (cost / base_cost[k] - 1.0)
            ));
        }
        println!(
            "\n{label}\n  total .pkon {tot_file} B ({:+.3}% vs baseline)  Stage3 model {tot_cost:.0} bits ({:+.3}%)  zeros e {:.1}% -> r {:.1}%\n  transform enc {:.1} ms dec {:.1} ms (baseline Stage 2: {:.1} / {:.1} ms); round-trip exact, forward deterministic\n{per}",
            100.0 * (tot_file as f64 / bf as f64 - 1.0),
            100.0 * (tot_cost / bc - 1.0),
            100.0 * zero_e as f64 / cnt as f64,
            100.0 * zero_r as f64 / cnt as f64,
            te * 1e3,
            td * 1e3,
            base_t.0 * 1e3,
            base_t.1 * 1e3
        );
    }
}
