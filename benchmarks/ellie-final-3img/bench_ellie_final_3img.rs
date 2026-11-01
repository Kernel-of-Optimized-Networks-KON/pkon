//! Final three-image production benchmark of the ELLIE container (codec 6): untransformed / YCoCg-R / auto.
//! Same corpus, 1024-px centre crop and canonical raw bytes as exp_ellie_ycocg. Measurement only.
//!   cargo run --release --bin bench_ellie_final_3img
use nanoscope::canon::{first_mismatch, Canonical, Spec};
use nanoscope::pkon::{self, EllieRepr};
use std::fmt::Write as _;
use std::path::PathBuf;
use std::time::Instant;

const CROP: u32 = 1024;
const REPS: usize = 5;
const BASE: [(&str, usize); 3] = [("portrait_0", 987_160), ("screen_0", 527_075), ("night_0", 2_057_866)];

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
    let dir = PathBuf::from("benchmarks/ellie-final-3img/run-final-three-image");
    std::fs::create_dir_all(dir.join("files")).unwrap();
    let modes = [("untransformed", EllieRepr::Untransformed), ("ycocg-r", EllieRepr::YCoCgR), ("auto", EllieRepr::Auto)];
    let mut md = String::from("# ELLIE final three-image production benchmark\n\nRelease build, container codec 6, best of 5 encodes/decodes per cell. Raw = canonical decoded 8-bit RGB bytes of the 1024-px centre crop.\n\n| image | mode | file B | payload B | overhead B | ratio (raw/file) | encode s | decode s | repr | roundtrip | deterministic |\n|---|---|---|---|---|---|---|---|---|---|---|\n");
    let mut tot = [[0usize; 2]; 3];
    let (mut te, mut td) = ([0f64; 3], [0f64; 3]);
    let mut raw_t = 0usize;
    let mut ok = true;
    for (name, base) in BASE {
        let ext = if name == "screen_0" { "png" } else { "jpg" };
        let img = crop(Canonical::load_bytes(std::fs::read(format!("data/{name}.{ext}")).unwrap()).unwrap());
        let raw = img.data.len();
        raw_t += raw;
        let mut sizes = [0usize; 3];
        for (mi, (label, mode)) in modes.iter().enumerate() {
            let (mut e, mut d) = (f64::MAX, f64::MAX);
            let mut file = vec![];
            let mut det = true;
            let mut exact = true;
            for rep in 0..REPS {
                let t = Instant::now();
                let f = pkon::encode_standard_with(&img, *mode).unwrap();
                e = e.min(t.elapsed().as_secs_f64());
                if rep == 0 { file = f } else { det &= f == file }
                let t = Instant::now();
                let back = pkon::decode(&file).unwrap();
                d = d.min(t.elapsed().as_secs_f64());
                let pkon::Decoded::Standard(back) = back else { panic!("not standard") };
                exact &= back.data == img.data && back.spec == img.spec && first_mismatch(&img, &back).is_none();
            }
            // Re-decode the written file from disk and check metadata against the source and the header.
            let path = dir.join("files").join(format!("{name}.{label}.pkon"));
            std::fs::write(&path, &file).unwrap();
            let disk = std::fs::read(&path).unwrap();
            let h = pkon::parse_header(&disk).unwrap();
            let pkon::Decoded::Standard(back) = pkon::decode(&disk).unwrap() else { panic!() };
            exact &= back.data == img.data && back.spec == img.spec;
            exact &= h.width == img.spec.width && h.height == img.spec.height && h.bit_depth == img.spec.bit_depth && h.channels as usize == img.spec.layout.channels() && back.spec.layout == img.spec.layout;
            ok &= exact && det;
            let overhead = pkon::FIXED_HEADER_LEN + h.meta_len as usize;
            let repr = file[pkon::FIXED_HEADER_LEN + 1];
            let _ = writeln!(md, "| {name} | {label} | {} | {} | {overhead} | {:.3} | {e:.3} | {d:.3} | {repr} | {exact} | {det} |", file.len(), file.len() - overhead, raw as f64 / file.len() as f64);
            tot[mi][0] += file.len();
            tot[mi][1] += file.len() - overhead;
            te[mi] += e;
            td[mi] += d;
            sizes[mi] = file.len();
            if !(exact && det) {
                eprintln!("FAILURE {name} {label}: exact={exact} det={det}");
            }
        }
        eprintln!("{name}: {sizes:?} (baseline {base})");
    }
    let bt: usize = BASE.iter().map(|b| b.1).sum();
    let _ = writeln!(md, "\n## Aggregate (summed)\n\nFrozen baseline total: {bt} B. Raw total: {raw_t} B.\n\n| mode | file B | payload B | saved vs baseline (file) | reduction | ratio (raw/file) | encode s (sum) | decode s (sum) |\n|---|---|---|---|---|---|---|---|");
    for (mi, (label, _)) in modes.iter().enumerate() {
        let s = bt as i64 - tot[mi][0] as i64;
        let _ = writeln!(md, "| {label} | {} | {} | {s} | {:.2}% | {:.3} | {:.3} | {:.3} |", tot[mi][0], tot[mi][1], s as f64 / bt as f64 * 100.0, raw_t as f64 / tot[mi][0] as f64, te[mi], td[mi]);
    }
    let _ = writeln!(md, "\nAll round-trip and determinism checks passed: {ok}.");
    std::fs::write(dir.join("report.md"), &md).unwrap();
    println!("{md}");
    if !ok {
        std::process::exit(1);
    }
}
