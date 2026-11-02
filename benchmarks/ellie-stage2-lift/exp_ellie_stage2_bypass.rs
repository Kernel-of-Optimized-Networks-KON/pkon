//! ELLIE Stage 2 bypass canary: production Stage 2 vs identity on the 3-image corpus. Diagnostic only; no production source is modified.
//!
//!   cargo run --release --bin exp_ellie_stage2_bypass
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

struct Img { name: String, w: usize, h: usize, bits: Vec<u32>, file_len: usize, payload_len: usize, prod: Vec<u8>, img: Canonical, planes: Vec<Vec<u16>> }

#[derive(Default, Clone)]
struct Res { payload: Vec<Vec<u8>>, cost: f64, enc: f64, dec: f64 }

/// Full chain for one image: Stage1 -> (Stage2 | identity) -> Stage3 -> Stage4, then back, checking pixel identity.
fn run(im: &Img, stage2: bool) -> Res {
    let mut r = Res::default();
    let mut s1 = [Stage1::new(im.w, im.h, 8).unwrap(), Stage1::new(im.w, im.h, 9).unwrap()];
    let mut enc = Stage4Enc::with_capacity(im.w * im.h);
    let t = Instant::now();
    let mut rs = vec![];
    for (pi, pl) in im.planes.iter().enumerate() {
        let mut e = vec![0i32; im.w * im.h];
        s1[(im.bits[pi] == 9) as usize].encode(pl, &mut e).unwrap();
        if stage2 { Stage2::new().encode_in_place(&mut e); }
        let mut s3 = Stage3::with_profile(im.w, im.h, im.bits[pi], PROFILE).unwrap();
        enc.reset();
        s3.encode_plane(&e, &mut enc).unwrap();
        r.payload.push(enc.finish().to_vec());
        rs.push(e);
    }
    r.enc = t.elapsed().as_secs_f64();
    for (pi, e) in rs.iter().enumerate() { r.cost += s3_cost(e, im.w, im.h, im.bits[pi]); }
    let mut s1 = [Stage1::new(im.w, im.h, 8).unwrap(), Stage1::new(im.w, im.h, 9).unwrap()];
    let t = Instant::now();
    let mut outs = vec![];
    for (pi, bytes) in r.payload.iter().enumerate() {
        let mut s3 = Stage3::with_profile(im.w, im.h, im.bits[pi], PROFILE).unwrap();
        let mut dec = Stage4Dec::new(bytes);
        let mut e = vec![0i32; im.w * im.h];
        s3.decode_plane(&mut e, &mut dec).unwrap();
        if stage2 { Stage2::new().decode_in_place(&mut e); }
        let mut out = vec![0u16; im.w * im.h];
        s1[(im.bits[pi] == 9) as usize].decode(&e, &mut out).unwrap();
        outs.push(out);
    }
    r.dec = t.elapsed().as_secs_f64();
    for (pi, o) in outs.iter().enumerate() { assert_eq!(o, &im.planes[pi], "PIXEL MISMATCH {} plane {pi} stage2={stage2}", im.name); }
    r
}

fn main() {
    if cfg!(debug_assertions) { eprintln!("use --release"); std::process::exit(2); }
    let data = std::path::PathBuf::from("data");
    let mut imgs = vec![];
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
        imgs.push(Img { name: f.split('.').next().unwrap().into(), w, h, bits, file_len: prod.len(), payload_len, prod, img, planes });
    }
    // production container round trip (baseline correctness of the real .pkon path)
    for im in &imgs {
        let pkon::Decoded::Standard(d) = pkon::decode(&im.prod).unwrap() else { panic!("not standard") };
        assert_eq!(d.data, im.img.data, "production decode mismatch {}", im.name);
    }
    const REPS: usize = 3;
    let mut tot = [0usize; 2];
    let mut cost = [0f64; 2];
    let mut tm = [(0f64, 0f64); 2];
    let mut per = vec![[0usize; 2]; imgs.len()];
    let mut percost = vec![[0f64; 2]; imgs.len()];
    for (k, im) in imgs.iter().enumerate() {
        for v in 0..2 {
            let stage2 = v == 0;
            let first = run(im, stage2);
            let (mut be, mut bd) = (first.enc, first.dec);
            for _ in 1..REPS {
                let again = run(im, stage2);
                assert_eq!(again.payload, first.payload, "NON-DETERMINISTIC {} stage2={stage2}", im.name);
                be = be.min(again.enc); bd = bd.min(again.dec);
            }
            let pl: usize = first.payload.iter().map(|p| p.len()).sum();
            if stage2 { assert_eq!(pl, im.payload_len, "rebuilt baseline payload != production payload for {}", im.name); }
            let file = im.file_len - im.payload_len + pl;
            per[k][v] = file; percost[k][v] = first.cost;
            tot[v] += file; cost[v] += first.cost; tm[v].0 += be; tm[v].1 += bd;
        }
    }
    println!("all decodes pixel-identical; {REPS} runs/variant byte-identical; baseline payload rebuilt == production payload");
    println!("{:<10} {:>10} {:>10} {:>8} {:>13} {:>13} {:>8}", "image", "base B", "bypass B", "delta%", "base bits", "bypass bits", "delta%");
    for (k, im) in imgs.iter().enumerate() {
        println!("{:<10} {:>10} {:>10} {:>+8.3} {:>13.0} {:>13.0} {:>+8.3}", im.name, per[k][0], per[k][1],
            100.0 * (per[k][1] as f64 / per[k][0] as f64 - 1.0), percost[k][0], percost[k][1], 100.0 * (percost[k][1] / percost[k][0] - 1.0));
    }
    println!("{:<10} {:>10} {:>10} {:>+8.3} {:>13.0} {:>13.0} {:>+8.3}", "TOTAL", tot[0], tot[1],
        100.0 * (tot[1] as f64 / tot[0] as f64 - 1.0), cost[0], cost[1], 100.0 * (cost[1] / cost[0] - 1.0));
    println!("bytes saved {} ({:.3}%)", tot[0] as i64 - tot[1] as i64, 100.0 * (tot[0] as f64 - tot[1] as f64) / tot[0] as f64);
    println!("encode (S1..S4, best of {REPS}): base {:.1} ms, bypass {:.1} ms; decode: base {:.1} ms, bypass {:.1} ms", tm[0].0*1e3, tm[1].0*1e3, tm[0].1*1e3, tm[1].1*1e3);
}
