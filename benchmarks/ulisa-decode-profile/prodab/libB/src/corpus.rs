//! Deterministic synthetic corpus + loader for external images.
//!
//! Synthetic images diagnose codec behaviour; they say little about natural images. Reports
//! label them `synthetic`. External images (`from_dir`) go through the same `Entry` type, so
//! the bench needs no changes to use them.
use crate::canon::{Canonical, Layout, Spec};
use std::path::Path;

pub struct Entry {
    pub name: String,
    pub kind: &'static str, // "synthetic" | "external"
    pub description: String,
    pub image: Canonical,
    /// Size of the original file for external images; synthetic images have no source file.
    pub source_bytes: Option<u64>,
}

struct Rng(u64);
impl Rng {
    /// splitmix64: tiny, fixed algorithm, so fixtures never change under us.
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

/// Build an image by filling each pixel's samples (as u16, scaled to the bit depth by the caller).
fn make(w: u32, h: u32, layout: Layout, depth: u8, mut f: impl FnMut(u32, u32, &mut [u16])) -> Canonical {
    let ch = layout.channels();
    let mut data = Vec::with_capacity(w as usize * h as usize * ch * (depth as usize / 8));
    let mut px = vec![0u16; ch];
    for y in 0..h {
        for x in 0..w {
            f(x, y, &mut px);
            for &s in &px {
                if depth == 8 {
                    data.push(s as u8);
                } else {
                    data.extend(s.to_le_bytes());
                }
            }
        }
    }
    Canonical::new(Spec { width: w, height: h, layout, bit_depth: depth }, data).expect("generator produced bad image")
}

fn clamp(v: i32, max: i32) -> u16 {
    v.clamp(0, max) as u16
}

pub fn synthetic() -> Vec<Entry> {
    let mut e: Vec<(&str, &str, Canonical)> = vec![];

    e.push(("flat_rgb", "single flat colour", make(64, 64, Layout::Rgb, 8, |_, _, p| p.copy_from_slice(&[200, 120, 40]))));

    e.push(("checker_rgb", "8px two-colour checkerboard", make(64, 64, Layout::Rgb, 8, |x, y, p| {
        let on = ((x / 8) + (y / 8)) % 2 == 0;
        p.copy_from_slice(if on { &[250, 250, 250] } else { &[10, 30, 90] });
    })));

    let mut r = Rng(1);
    let tile: Vec<[u16; 3]> = (0..16).map(|_| [r.below(256) as u16, r.below(256) as u16, r.below(256) as u16]).collect();
    e.push(("tile_rgb", "random 4x4 tile repeated (exact 2D repetition)", make(64, 64, Layout::Rgb, 8, |x, y, p| {
        p.copy_from_slice(&tile[((y % 4) * 4 + x % 4) as usize]);
    })));

    e.push(("gradient_rgb", "smooth 2D colour gradient", make(128, 96, Layout::Rgb, 8, |x, y, p| {
        p.copy_from_slice(&[(x * 255 / 127) as u16, (y * 255 / 95) as u16, ((x + y) * 255 / 222) as u16]);
    })));

    e.push(("gradient_gray_odd", "gray gradient, odd 97x61", make(97, 61, Layout::Gray, 8, |x, y, p| {
        p[0] = ((x + y) * 255 / 156) as u16;
    })));

    e.push(("shapes_rgb", "flat shapes with hard edges (rect, disc, diagonal, triangle)", make(128, 128, Layout::Rgb, 8, |x, y, p| {
        let (xi, yi) = (x as i32, y as i32);
        let mut c = [245, 245, 235];
        if (20..60).contains(&xi) && (20..50).contains(&yi) { c = [30, 60, 200]; }
        if (xi - 90).pow(2) + (yi - 40).pow(2) < 25 * 25 { c = [210, 40, 40]; }
        if (xi - yi).abs() < 2 { c = [0, 0, 0]; }
        if yi > 80 && xi > 20 && (xi - 20) < (yi - 80) * 2 && yi < 120 { c = [40, 170, 60]; }
        p.copy_from_slice(&c);
    })));

    let mut r = Rng(2);
    e.push(("noise_rgb", "uniform random RGB (incompressible)", make(64, 64, Layout::Rgb, 8, |_, _, p| {
        for s in p.iter_mut() { *s = r.below(256) as u16; }
    })));

    let mut r = Rng(3);
    e.push(("correlated_rgb", "smooth base; G and B track R plus small noise", make(96, 96, Layout::Rgb, 8, |x, y, p| {
        let base = ((x * 2 + y) % 200 + 20) as i32;
        let n = |r: &mut Rng| r.below(5) as i32 - 2;
        p.copy_from_slice(&[clamp(base + n(&mut r), 255), clamp(base + 6 + n(&mut r), 255), clamp(base / 2 + n(&mut r), 255)]);
    })));

    e.push(("odd_rgb", "odd dimensions 33x17 mixed content", make(33, 17, Layout::Rgb, 8, |x, y, p| {
        p.copy_from_slice(&[(x * 7 % 256) as u16, (y * 15) as u16, ((x * y) % 256) as u16]);
    })));

    e.push(("alpha_rgba", "gradient colour, soft radial alpha, opaque disc", make(80, 80, Layout::Rgba, 8, |x, y, p| {
        let d2 = (x as i32 - 40).pow(2) + (y as i32 - 40).pow(2);
        let a = if d2 < 10 * 10 { 255 } else { clamp(255 - (d2 as f64).sqrt() as i32 * 6, 255) };
        p.copy_from_slice(&[(x * 3) as u16, (y * 3) as u16, 128, a]);
    })));

    let mut r = Rng(4);
    e.push(("alpha_hidden_rgba", "left half alpha=0 but random RGB (hidden data); right half opaque gradient", make(64, 64, Layout::Rgba, 8, |x, y, p| {
        if x < 32 {
            p.copy_from_slice(&[r.below(256) as u16, r.below(256) as u16, r.below(256) as u16, 0]);
        } else {
            p.copy_from_slice(&[(x * 4) as u16, (y * 4) as u16, 90, 255]);
        }
    })));

    let mut r = Rng(5);
    e.push(("noise_rgba", "uniform random RGBA incl. random alpha (incompressible, hostile to alpha handling)", make(48, 48, Layout::Rgba, 8, |_, _, p| {
        for s in p.iter_mut() { *s = r.below(256) as u16; }
    })));

    e.push(("graya_gradient", "gray+alpha gradient", make(40, 30, Layout::GrayAlpha, 8, |x, y, p| {
        p.copy_from_slice(&[(x * 6) as u16, (y * 8) as u16]);
    })));

    let mut r = Rng(6);
    e.push(("gray16_gradient", "16-bit gray gradient with 2-bit noise", make(64, 64, Layout::Gray, 16, |x, y, p| {
        p[0] = clamp(((x + y) * 65535 / 126) as i32 + r.below(4) as i32, 65535);
    })));

    let mut r = Rng(7);
    e.push(("rgb16_smooth", "16-bit RGB smooth + 4-bit noise", make(48, 48, Layout::Rgb, 16, |x, y, p| {
        for (c, s) in p.iter_mut().enumerate() {
            *s = clamp(((x * (c as u32 + 1) + y * 3) * 300) as i32 + r.below(16) as i32, 65535);
        }
    })));

    e.into_iter()
        .map(|(n, d, image)| Entry { name: n.into(), kind: "synthetic", description: d.into(), image, source_bytes: None })
        .collect()
}

/// Load every PNG in `dir` (non-recursive, sorted). Returns entries plus (file, reason)
/// for files that could not be represented — recorded, not converted.
pub fn from_dir(dir: &Path) -> Result<(Vec<Entry>, Vec<(String, String)>), String> {
    let mut paths: Vec<_> = std::fs::read_dir(dir)
        .map_err(|e| format!("{}: {e}", dir.display()))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().map_or(false, |x| x.eq_ignore_ascii_case("png")))
        .collect();
    paths.sort();
    let (mut ok, mut skipped) = (vec![], vec![]);
    for p in paths {
        let name = p.file_name().unwrap().to_string_lossy().to_string();
        match Canonical::load(&p) {
            Ok(image) => ok.push(Entry {
                name,
                kind: "external",
                description: image.notes.join("; "),
                source_bytes: std::fs::metadata(&p).ok().map(|m| m.len()),
                image,
            }),
            Err(why) => skipped.push((name, why)),
        }
    }
    Ok((ok, skipped))
}
