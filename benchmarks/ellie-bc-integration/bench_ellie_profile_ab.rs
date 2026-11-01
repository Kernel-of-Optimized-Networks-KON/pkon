//! Production A/B for the ELLIE Stage 3 profile: Legacy4 (previous production) vs Bc255S11 (U-BC-l255s11), both through the
//! real `.pkon` container (`encode_standard_profile(.., Auto, ..)` / `decode`). Same corpus and 1024-px centre crop as the
//! Pareto experiment. Profiles are interleaved with a rotating order per repetition; the reported time per image is the
//! minimum over repetitions. File sizes are the actual `.pkon` byte counts.
//!   bench_ellie_profile_ab --set 3|50 --reps N --out <dir>
//! Writes <dir>/<profile>/<image>.pkon and <dir>/results.csv.
use nanoscope::canon::{Canonical, Spec};
use nanoscope::ellie::Stage3Profile;
use nanoscope::pkon::{self, EllieRepr};
use std::path::PathBuf;
use std::time::Instant;

fn center_crop(img: &Canonical, max: u32) -> Canonical {
    let (w, h) = (img.spec.width, img.spec.height);
    if w <= max && h <= max {
        return img.clone();
    }
    let (cw, ch) = (w.min(max), h.min(max));
    let (x0, y0) = ((w - cw) / 2, (h - ch) / 2);
    let row = img.spec.layout.channels() * img.spec.bytes_per_sample();
    let mut data = Vec::with_capacity(cw as usize * ch as usize * row);
    for y in y0..y0 + ch {
        let s = (y as usize * w as usize + x0 as usize) * row;
        data.extend_from_slice(&img.data[s..s + cw as usize * row]);
    }
    Canonical::new(Spec { width: cw, height: ch, ..img.spec }, data).unwrap()
}

fn load(set: &str) -> Vec<(String, Canonical)> {
    let data = PathBuf::from("data");
    if set == "3" {
        return ["portrait_0.jpg", "screen_0.png", "night_0.jpg"]
            .iter()
            .map(|f| (f.split('.').next().unwrap().to_string(), center_crop(&Canonical::load(&data.join(f)).unwrap(), 1024)))
            .collect();
    }
    let m: Vec<serde_json::Value> = serde_json::from_str(&std::fs::read_to_string(data.join("dataset50.json")).unwrap()).unwrap();
    m.iter().map(|m| (m["name"].as_str().unwrap().to_string(), center_crop(&Canonical::load(&data.join(m["file"].as_str().unwrap())).unwrap(), 1024))).collect()
}

fn main() {
    if cfg!(debug_assertions) {
        eprintln!("use --release");
        std::process::exit(2);
    }
    let a: Vec<String> = std::env::args().collect();
    let arg = |k: &str| a.iter().position(|x| x == k).map(|i| a[i + 1].clone());
    let set = arg("--set").unwrap_or("3".into());
    let reps: usize = arg("--reps").unwrap_or("5".into()).parse().unwrap();
    let out = PathBuf::from(arg("--out").unwrap());
    let profiles = [("legacy4", Stage3Profile::Legacy4), ("bc255s11", Stage3Profile::Bc255S11)];
    for (n, _) in profiles {
        std::fs::create_dir_all(out.join(n)).unwrap();
    }
    let mut csv = String::from("image,profile,raw_bytes,file_bytes,enc_min_s,dec_min_s,exact,det,repr\n");
    for (name, img) in load(&set) {
        let raw = img.data.len();
        let mut enc = [f64::MAX; 2];
        let mut dec = [f64::MAX; 2];
        let mut files: [Vec<u8>; 2] = [vec![], vec![]];
        let (mut exact, mut det) = ([true; 2], [true; 2]);
        for rep in 0..reps {
            for j in 0..2 {
                let c = (j + rep) % 2;
                let t = Instant::now();
                let f = pkon::encode_standard_profile(&img, EllieRepr::Auto, profiles[c].1).unwrap();
                enc[c] = enc[c].min(t.elapsed().as_secs_f64());
                let t = Instant::now();
                let back = pkon::decode(&f).unwrap();
                dec[c] = dec[c].min(t.elapsed().as_secs_f64());
                let pkon::Decoded::Standard(back) = back else { panic!("not standard") };
                exact[c] &= back.data == img.data && back.spec == img.spec;
                if rep == 0 { files[c] = f } else { det[c] &= files[c] == f }
            }
        }
        for c in 0..2 {
            std::fs::write(out.join(profiles[c].0).join(format!("{name}.pkon")), &files[c]).unwrap();
            let h = pkon::parse_header(&files[c]).unwrap();
            let pkon::CodecParams::Ellie(p) = h.params else { panic!() };
            csv += &format!("{name},{},{raw},{},{:.6},{:.6},{},{},{}\n", profiles[c].0, files[c].len(), enc[c], dec[c], exact[c], det[c], p.representation);
        }
        eprintln!("{name}: legacy4 {} B  bc255s11 {} B", files[0].len(), files[1].len());
    }
    std::fs::write(out.join("results.csv"), csv).unwrap();
}
