//! Diagnostic: splits the context-mixing model cost from the predictor and measures the flank stage's cost, public API only.
//! Residuals and structural leaves of the decoded image are computed once (Stage1::encode_tree), flank bases are rebuilt here
//! from the same definition as `ellie::flank_bases`, and the Stage 3 model is then replayed over them in isolation:
//!   Mo_fl   Stage3 with flank attached, NullIo     (model + flank cost, no predictor, no range coder)
//!   Mo_nofl Stage3 without flank, NullIo           (what ULISA's model would cost with the flank stage off; different stream, timing only)
//!   Mo_rc   Stage3 with flank, Stage4Enc           (adds the range coder)
//!   Bases   flank base construction, harness copy  (upper bound for production `flank_bases`, which uses slice fast paths)
//! usage: model <file.pkon> [reps]
use nanoscope::ellie::{flank_base, FlankModel, FlankTopology, Stage1, Stage3, Stage3Profile, Stage4Enc};
use nanoscope::lisa_frame::BayerFrame;
use nanoscope::pkon::{self, CodecParams, Decoded};
use nanoscope::rangecoder::BitIo;
use std::time::Instant;

struct NullIo;
impl BitIo for NullIo {
    #[inline(always)]
    fn code(&mut self, _p: u32, bit: u32) -> u32 {
        bit
    }
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

fn bases_of(topo: &FlankTopology, dims: &[(usize, usize); 4], stored: &[Vec<i16>; 4], k: usize) -> Vec<u8> {
    let (w, h) = dims[k];
    let mut sums = vec![0i32; w * h];
    for l in topo.links(k) {
        let (sw, sh) = (dims[l.plane as usize].0 as isize, dims[l.plane as usize].1 as isize);
        let src = &stored[l.plane as usize];
        let at = |x: isize, y: isize| src[(y.clamp(0, sh - 1) * sw + x.clamp(0, sw - 1)) as usize] as i32;
        for y in 0..h {
            for x in 0..w {
                sums[y * w + x] += at(x as isize + l.a.0 as isize, y as isize + l.a.1 as isize) + at(x as isize + l.b.0 as isize, y as isize + l.b.1 as isize);
            }
        }
    }
    sums.iter().map(|&v| flank_base(v) as u8).collect()
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let bytes = std::fs::read(&a[1]).unwrap();
    let reps: usize = a.get(2).and_then(|s| s.parse().ok()).unwrap_or(3);
    let hdr = pkon::parse_header(&bytes).unwrap();
    let CodecParams::Ulisa(p) = &hdr.params else { panic!("not codec 7") };
    let frame = BayerFrame::with_lattice(p.metadata.clone(), hdr.bit_depth, p.lattice).unwrap();
    let depth = u32::from(frame.coding_depth());
    let dims = frame.plane_dims();
    let n = hdr.width as f64 * hdr.height as f64;
    let Decoded::Raw(img) = pkon::decode(&bytes).unwrap() else { panic!() };
    assert!(p.lattice.is_none());
    let planes = frame.split(&img.samples);
    let topo = frame.topology().unwrap();

    // Residuals, leaves and bases, built once.
    let mut res: Vec<Vec<i32>> = Vec::new();
    let mut leaf: Vec<Vec<u8>> = Vec::new();
    let mut stored: [Vec<i16>; 4] = Default::default();
    let mut bases: Vec<Vec<u8>> = Vec::new();
    for k in 0..4 {
        let (w, h) = dims[k];
        let mut s1 = Stage1::new(w, h, depth).unwrap();
        let (mut r, mut l) = (vec![0i32; w * h], vec![0u8; w * h]);
        s1.encode_tree(&planes[k], &mut r, &mut l).unwrap();
        bases.push(bases_of(&topo, &dims, &stored, k));
        stored[k] = r.iter().map(|&v| v.clamp(i16::MIN as i32, i16::MAX as i32) as i16).collect();
        res.push(r);
        leaf.push(l);
    }
    // Leaf histogram (structural tree) for the report.
    let mut lh = [0u64; 12];
    for l in &leaf {
        for &v in l {
            lh[v as usize] += 1;
        }
    }
    println!("leaf histogram (%): {:?}", lh.iter().map(|&c| (1000.0 * c as f64 / n).round() / 10.0).collect::<Vec<_>>());

    let run = |flank: bool, io_real: bool| {
        let mut fm = FlankModel::new();
        let mut enc = Stage4Enc::new();
        for k in 0..4 {
            let (w, h) = dims[k];
            let mut s3 = Stage3::with_profile(w, h, depth, Stage3Profile::Bc255S11Tree).unwrap();
            if flank {
                s3.attach_flank(std::mem::take(&mut fm));
                if io_real {
                    s3.encode_plane_leaf_flank(&res[k], &leaf[k], &mut enc, &bases[k]).unwrap();
                } else {
                    s3.encode_plane_leaf_flank(&res[k], &leaf[k], &mut NullIo, &bases[k]).unwrap();
                }
                fm = s3.detach_flank();
            } else {
                s3.encode_plane_leaf(&res[k], &leaf[k], &mut NullIo).unwrap();
            }
        }
        enc.finish().len()
    };
    let t_fl = best(reps, || run(true, false));
    let t_nofl = best(reps, || run(false, false));
    let t_rc = best(reps, || run(true, true));
    let t_b = best(reps, || {
        let mut st: [Vec<i16>; 4] = Default::default();
        for k in 0..4 {
            std::hint::black_box(bases_of(&topo, &dims, &st, k));
            st[k] = stored[k].clone();
        }
    });
    println!("model-only timings (best of {reps}, s): Mo_fl {t_fl:.3} | Mo_nofl {t_nofl:.3} | Mo_rc {t_rc:.3} | Bases(harness copy) {t_b:.3}");
    println!("  flank stage adds {:.3}s ({:.1}% of Mo_fl); range coder (encode) adds {:.3}s", t_fl - t_nofl, 100.0 * (t_fl - t_nofl) / t_fl, t_rc - t_fl);
    println!("  Mo_fl {:.1} ns/sample", t_fl / n * 1e9);
}
