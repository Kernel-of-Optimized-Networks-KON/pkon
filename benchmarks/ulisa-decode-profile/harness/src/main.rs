//! Diagnostic: ULISA codec 7 decode, phase breakdown through the public API only (no production code touched).
//! usage: phases <file.pkon> [reps]
use nanoscope::ellie::FlankCodec;
use nanoscope::lisa_frame::BayerFrame;
use nanoscope::pkon::{self, CodecParams};
use std::time::Instant;

fn t<R>(f: impl FnOnce() -> R) -> (f64, R) {
    let s = Instant::now();
    let r = f();
    (s.elapsed().as_secs_f64(), r)
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let bytes = std::fs::read(&a[1]).unwrap();
    let reps: usize = a.get(2).and_then(|s| s.parse().ok()).unwrap_or(3);
    let hdr = pkon::parse_header(&bytes).unwrap();
    let CodecParams::Ulisa(p) = &hdr.params else { panic!("not codec 7") };
    let end = hdr.file_len() as usize;
    let payload = &bytes[pkon::FIXED_HEADER_LEN + hdr.meta_len as usize..end];
    let n = hdr.width as u64 * hdr.height as u64;
    println!("file={} {}x{} bits={} samples={} payload={}B lattice={:?} bpp={:.3}", a[1], hdr.width, hdr.height, hdr.bit_depth, n, payload.len(), p.lattice, payload.len() as f64 * 8.0 / n as f64);

    for r in 0..reps {
        let (full, d) = t(|| pkon::decode(&bytes).unwrap());
        drop(d);
        // Same steps as pkon::decode_ulisa, timed separately.
        let (t_frame, frame) = t(|| {
            let f = BayerFrame::with_lattice(p.metadata.clone(), hdr.bit_depth, p.lattice).unwrap();
            f
        });
        let topo = frame.topology().unwrap();
        let (t_new, mut codec) = t(|| FlankCodec::new(frame.plane_dims(), u32::from(frame.coding_depth()), topo).unwrap());
        let (t_alloc, mut planes) = t(|| -> [Vec<u16>; 4] { std::array::from_fn(|k| vec![0u16; frame.plane_dims()[k].0 * frame.plane_dims()[k].1]) });
        let (t_dec, _) = t(|| {
            let [x, y, z, w] = &mut planes;
            codec.decode_planes(payload, [x, y, z, w]).unwrap();
        });
        let (t_merge, mut samples) = t(|| frame.merge(&planes));
        let (t_den, _) = t(|| frame.denormalize(&mut samples).unwrap());
        let (t_crc, crc) = t(|| pkon::pixel_crc_u16(&samples, hdr.bit_depth));
        assert_eq!(crc, hdr.pixel_crc32);
        println!(
            "rep{r}: full={full:.3}s ({:.3} Mpx/s) | frame={t_frame:.4} new={t_new:.4} alloc={t_alloc:.4} decode_planes={t_dec:.3} ({:.1}%) merge={t_merge:.4} denorm={t_den:.4} crc={t_crc:.4} (flank sums seen={}, nonzero={})",
            n as f64 / full / 1e6,
            100.0 * t_dec / full,
            codec.stats.samples,
            codec.stats.nonzero
        );
    }
}
