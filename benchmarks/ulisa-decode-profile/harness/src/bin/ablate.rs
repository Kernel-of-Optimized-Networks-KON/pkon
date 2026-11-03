//! Diagnostic ablations of the ULISA decoder, public API only. The decoded image is split into parity planes and re-encoded with
//! different `BitIo` back-ends, which isolates cost centres without touching production code:
//!   P  predictor + structural-tree leaf only (Stage1::encode_tree)
//!   M  full model (predictor + flank bases + context mixing + slot/weight updates), no range coder (NullIo)
//!   E  full encoder with the real range coder (Stage4Enc); its bytes must equal the stored payload
//!   D  full decoder (Stage4Dec): the production path
//! plus allocation counts (counting global allocator) and decision statistics (CountIo).
//! usage: ablate <file.pkon> [reps]
use nanoscope::ellie::{FlankCodec, Stage1, Stage3, Stage3Profile, Stage4Enc};
use nanoscope::lisa_frame::BayerFrame;
use nanoscope::pkon::{self, CodecParams, Decoded};
use nanoscope::rangecoder::BitIo;
use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::time::Instant;

struct CountAlloc;
static A_N: AtomicU64 = AtomicU64::new(0);
static A_B: AtomicU64 = AtomicU64::new(0);
static A_LIVE: AtomicU64 = AtomicU64::new(0);
static A_PEAK: AtomicU64 = AtomicU64::new(0);

fn note(size: u64) {
    A_N.fetch_add(1, Relaxed);
    A_B.fetch_add(size, Relaxed);
    let live = A_LIVE.fetch_add(size, Relaxed) + size;
    A_PEAK.fetch_max(live, Relaxed);
}

unsafe impl GlobalAlloc for CountAlloc {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        note(l.size() as u64);
        System.alloc(l)
    }
    unsafe fn alloc_zeroed(&self, l: Layout) -> *mut u8 {
        note(l.size() as u64);
        System.alloc_zeroed(l)
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        A_LIVE.fetch_sub(l.size() as u64, Relaxed);
        System.dealloc(p, l)
    }
    unsafe fn realloc(&self, p: *mut u8, l: Layout, n: usize) -> *mut u8 {
        A_N.fetch_add(1, Relaxed);
        A_B.fetch_add(n as u64, Relaxed);
        if n > l.size() {
            let live = A_LIVE.fetch_add((n - l.size()) as u64, Relaxed) + (n - l.size()) as u64;
            A_PEAK.fetch_max(live, Relaxed);
        } else {
            A_LIVE.fetch_sub((l.size() - n) as u64, Relaxed);
        }
        System.realloc(p, l, n)
    }
}
#[global_allocator]
static G: CountAlloc = CountAlloc;

struct NullIo;
impl BitIo for NullIo {
    #[inline(always)]
    fn code(&mut self, _p: u32, bit: u32) -> u32 {
        bit
    }
}

#[derive(Default)]
struct CountIo {
    calls: u64,
    raw: u64,
    ones: u64,
    bits_cost: f64,
    exp_miss: f64,
    /// Decisions by min(p, 1-p): <1%, <2.5%, <5%, <10%, <20%, <30%, <40%, >=40%.
    hist: [u64; 8],
}
impl BitIo for CountIo {
    fn code(&mut self, p1: u32, bit: u32) -> u32 {
        self.calls += 1;
        self.ones += bit as u64;
        let p = p1 as f64 / 65536.0;
        if p1 == 32768 {
            self.raw += 1;
        }
        self.bits_cost -= if bit != 0 { p.log2() } else { (1.0 - p).log2() };
        let m = p.min(1.0 - p);
        self.exp_miss += m;
        let b = [0.01, 0.025, 0.05, 0.10, 0.20, 0.30, 0.40].iter().position(|&t| m < t).unwrap_or(7);
        self.hist[b] += 1;
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

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let bytes = std::fs::read(&a[1]).unwrap();
    let reps: usize = a.get(2).and_then(|s| s.parse().ok()).unwrap_or(2);
    let hdr = pkon::parse_header(&bytes).unwrap();
    let CodecParams::Ulisa(p) = &hdr.params else { panic!("not codec 7") };
    let payload = &bytes[pkon::FIXED_HEADER_LEN + hdr.meta_len as usize..hdr.file_len() as usize];
    let n = hdr.width as f64 * hdr.height as f64;
    let frame = BayerFrame::with_lattice(p.metadata.clone(), hdr.bit_depth, p.lattice).unwrap();
    let depth = u32::from(frame.coding_depth());
    let dims = frame.plane_dims();

    // Allocation activity during one production decode.
    let (n0, b0) = (A_N.load(Relaxed), A_B.load(Relaxed));
    let base_live = A_LIVE.load(Relaxed);
    A_PEAK.store(base_live, Relaxed);
    let t = Instant::now();
    let Decoded::Raw(img) = pkon::decode(&bytes).unwrap() else { panic!() };
    let t_full = t.elapsed().as_secs_f64();
    println!(
        "alloc during decode: {} allocations, {:.1} MB requested in total, peak live +{:.1} MB (output buffer {:.1} MB)",
        A_N.load(Relaxed) - n0,
        (A_B.load(Relaxed) - b0) as f64 / 1e6,
        (A_PEAK.load(Relaxed) - base_live) as f64 / 1e6,
        img.samples.len() as f64 * 2.0 / 1e6,
    );
    assert!(p.lattice.is_none(), "harness assumes no L1 lattice");
    let planes = frame.split(&img.samples);
    let pl: [&[u16]; 4] = [&planes[0], &planes[1], &planes[2], &planes[3]];
    let topo = frame.topology().unwrap();
    drop(img);

    // D: production decode_planes in isolation.
    let t_d = best(reps, || {
        let mut codec = FlankCodec::new(dims, depth, topo.clone()).unwrap();
        let mut out: [Vec<u16>; 4] = std::array::from_fn(|k| vec![0u16; dims[k].0 * dims[k].1]);
        let [x, y, z, w] = &mut out;
        codec.decode_planes(payload, [x, y, z, w]).unwrap();
        out
    });
    // E: encoder with the real coder; must reproduce the stored payload byte for byte.
    let mut enc_bytes = Vec::new();
    let t_e = best(reps, || {
        let mut codec = FlankCodec::new(dims, depth, topo.clone()).unwrap();
        let mut enc = Stage4Enc::new();
        codec.encode_planes(pl, &mut enc).unwrap();
        enc_bytes = enc.finish().to_vec();
    });
    println!("replay-encode payload identical to stored payload: {} ({} B)", enc_bytes == payload, enc_bytes.len());
    // M: model without range coder.
    let t_m = best(reps, || {
        let mut codec = FlankCodec::new(dims, depth, topo.clone()).unwrap();
        codec.encode_planes(pl, &mut NullIo).unwrap();
    });
    // P: predictor and leaf only.
    let t_p = best(reps, || {
        for k in 0..4 {
            let (w, h) = dims[k];
            let mut s1 = Stage1::new(w, h, depth).unwrap();
            let (mut res, mut leaf) = (vec![0i32; w * h], vec![0u8; w * h]);
            s1.encode_tree(pl[k], &mut res, &mut leaf).unwrap();
        }
    });
    // Z: per-plane model allocation and initialisation.
    let t_z = best(reps, || {
        for k in 0..4 {
            std::hint::black_box(Stage3::with_profile(dims[k].0, dims[k].1, depth, Stage3Profile::Bc255S11Tree).unwrap());
        }
    });
    // Decision statistics.
    let mut c = CountIo::default();
    FlankCodec::new(dims, depth, topo.clone()).unwrap().encode_planes(pl, &mut c).unwrap();

    println!("samples={n:.0} bits={} (coding depth {depth}) payload={} B ({:.3} bpp)", hdr.bit_depth, payload.len(), payload.len() as f64 * 8.0 / n);
    println!("timings (best of {reps}, seconds):");
    println!("  production pkon::decode (1st call, cold)      {t_full:8.3}");
    println!("  D  decode_planes (Stage4Dec)                  {t_d:8.3}   {:6.1} ns/sample", t_d / n * 1e9);
    println!("  E  encode_planes (Stage4Enc)                  {t_e:8.3}   (D/E = {:.3})", t_d / t_e);
    println!("  M  encode_planes (NullIo, no range coder)     {t_m:8.3}   (E-M = {:.3}s)", t_e - t_m);
    println!("  P  predictor + leaf only                      {t_p:8.3}   ({:.1}% of D)", 100.0 * t_p / t_d);
    println!("  Z  4x Stage3::with_profile                    {t_z:8.4}");
    println!("  derived: model+flank (M-P)                    {:8.3}   ({:.1}% of D)", t_m - t_p, 100.0 * (t_m - t_p) / t_d);
    println!("  derived: D-M (range decode + dependency)      {:8.3}   ({:.1}% of D)", t_d - t_m, 100.0 * (t_d - t_m) / t_d);
    println!("decisions: {} total = {:.2}/sample; raw (p=1/2) {:.2}/sample; ones fraction {:.3}", c.calls, c.calls as f64 / n, c.raw as f64 / n, c.ones as f64 / c.calls as f64);
    println!("  modelled+sign {:.2}/sample; ideal cost {:.3} bpp (stored {:.3} bpp)", (c.calls - c.raw) as f64 / n, c.bits_cost / n, payload.len() as f64 * 8.0 / n);
    println!("  D: {:.2} ns/decision = {:.0} cycles/decision at 3.0 GHz nominal", t_d / c.calls as f64 * 1e9, t_d / c.calls as f64 * 3.0e9);
    println!("  expected data-dependent mispredicts (sum min(p,1-p)): {:.3}/sample = {:.1}% of decisions", c.exp_miss / n, 100.0 * c.exp_miss / c.calls as f64);
    println!("  min(p,1-p) histogram <1%,<2.5%,<5%,<10%,<20%,<30%,<40%,>=40% (% of decisions):");
    println!("   {:?}", c.hist.iter().map(|&h| (1000.0 * h as f64 / c.calls as f64).round() / 10.0).collect::<Vec<_>>());
}
