//! Production three-image benchmark worker for the Paeth A/B. Built twice (HEAD worktree = baseline, working tree = Paeth);
//! a driver alternates the two binaries. Same corpus, 1024-px centre crop and canonical raw bytes as bench_ellie_final_3img.
//!   bench_ellie_paeth_prod <out_dir> <reps>
//! Writes <out_dir>/<image>.auto.pkon and prints one line per image:
//!   name raw_bytes file_bytes repr exact det enc_auto_s enc_ycocg_s dec_s
use nanoscope::canon::{first_mismatch, Canonical, Spec};
use nanoscope::pkon::{self, EllieRepr};
use std::time::Instant;

const CROP: u32 = 1024;

fn crop(c: Canonical) -> Canonical {
    let Spec { width, height, layout, bit_depth } = c.spec;
    let (nw, nh) = (width.min(CROP), height.min(CROP));
    if (nw, nh) == (width, height) {
        return c;
    }
    let (x0, y0) = ((width - nw) as usize / 2, (height - nh) as usize / 2);
    let ch = layout.channels();
    let mut data = Vec::with_capacity(nw as usize * nh as usize * ch);
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
    let a: Vec<String> = std::env::args().collect();
    let dir = std::path::PathBuf::from(&a[1]);
    let reps: usize = a[2].parse().unwrap();
    std::fs::create_dir_all(&dir).unwrap();
    for name in ["portrait_0", "screen_0", "night_0"] {
        let ext = if name == "screen_0" { "png" } else { "jpg" };
        let img = crop(Canonical::load_bytes(std::fs::read(format!("data/{name}.{ext}")).unwrap()).unwrap());
        let (mut ea, mut ey, mut d) = (f64::MAX, f64::MAX, f64::MAX);
        let mut file = vec![];
        let (mut det, mut exact) = (true, true);
        for rep in 0..reps {
            let t = Instant::now();
            let f = pkon::encode_standard_with(&img, EllieRepr::Auto).unwrap();
            ea = ea.min(t.elapsed().as_secs_f64());
            let t = Instant::now();
            let fy = pkon::encode_standard_with(&img, EllieRepr::YCoCgR).unwrap();
            ey = ey.min(t.elapsed().as_secs_f64());
            if rep == 0 { file = f } else { det &= f == file }
            det &= fy == file; // auto must have selected YCoCg-R for these images; checked via repr byte below too
            let t = Instant::now();
            let back = pkon::decode(&file).unwrap();
            d = d.min(t.elapsed().as_secs_f64());
            let pkon::Decoded::Standard(back) = back else { panic!("not standard") };
            exact &= back.data == img.data && back.spec == img.spec && first_mismatch(&img, &back).is_none();
        }
        let path = dir.join(format!("{name}.auto.pkon"));
        std::fs::write(&path, &file).unwrap();
        let disk = std::fs::read(&path).unwrap();
        let h = pkon::parse_header(&disk).unwrap();
        let pkon::Decoded::Standard(back) = pkon::decode(&disk).unwrap() else { panic!() };
        exact &= back.data == img.data && back.spec == img.spec && back.spec.layout == img.spec.layout;
        exact &= h.width == img.spec.width && h.height == img.spec.height && h.bit_depth == img.spec.bit_depth && h.channels as usize == img.spec.layout.channels();
        println!("{name} {} {} {} {exact} {det} {ea:.4} {ey:.4} {d:.4}", img.data.len(), disk.len(), file[pkon::FIXED_HEADER_LEN + 1]);
    }
}
