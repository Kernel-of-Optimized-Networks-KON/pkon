//! T1 (read-only diagnostic): real-coder bytes of the OR-128 table and tree prefixes on the FULL 13 train images.
use nanoscope::lisa::*;
use nanoscope::lisa_bayer_plane::encode_stage2;
use nanoscope::lisa_or128_tree::{self as tree, FEATURE_NAMES, NF};
use nanoscope::rangecoder::{BinEnc, BitIo};
use serde_json::{json, Value};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

const E_MAX: usize = 33;
const LIMIT: usize = 255;

fn rates() -> &'static [u32; LIMIT + 1] {
    static R: std::sync::OnceLock<[u32; LIMIT + 1]> = std::sync::OnceLock::new();
    R.get_or_init(|| {
        let mut r = [0u32; LIMIT + 1];
        for (n, v) in r.iter_mut().enumerate() {
            *v = (65536.0 / (n as f64 + 1.5)) as u32;
        }
        r
    })
}
#[derive(Clone, Copy)]
struct B {
    p: u16,
    n: u8,
}
impl B {
    #[inline]
    fn upd(&mut self, bit: u32, r: &[u32; LIMIT + 1]) {
        let dp = (((bit as i32) << 16) - self.p as i32) as i64 * r[self.n as usize] as i64 >> 16;
        self.p = (self.p as i64 + dp).clamp(32, 65535 - 32) as u16;
        if (self.n as usize) < LIMIT {
            self.n += 1;
        }
    }
}
#[derive(Clone, Copy)]
struct M {
    unary: [B; E_MAX + 1],
    mant: [B; E_MAX + 1],
}
impl M {
    fn new() -> Self {
        let b = B { p: 32768, n: 0 };
        M { unary: [b; E_MAX + 1], mant: [b; E_MAX + 1] }
    }
    #[inline]
    fn bit<I: BitIo>(io: &mut I, b: &mut B, bit: u32, r: &[u32; LIMIT + 1]) -> u32 {
        let bit = io.code(b.p as u32, bit);
        b.upd(bit, r);
        bit
    }
    #[inline]
    fn code<I: BitIo>(&mut self, io: &mut I, v: u32, r: &[u32; LIMIT + 1]) {
        let x = u64::from(v) + 1;
        let e_in = (64 - x.leading_zeros()) as usize;
        let mut e = E_MAX;
        for i in 1..E_MAX {
            if Self::bit(io, &mut self.unary[i], (e_in > i) as u32, r) == 0 {
                e = i;
                break;
            }
        }
        for j in (0..e - 1).rev() {
            let want = ((x >> j) & 1) as u32;
            if j == e - 2 {
                Self::bit(io, &mut self.mant[e], want, r);
            } else {
                io.code(32768, want);
            }
        }
    }
}
struct CostIo<'a> {
    bits: f64,
    lg: &'a [f32],
}
impl BitIo for CostIo<'_> {
    #[inline]
    fn code(&mut self, p1: u32, bit: u32) -> u32 {
        let p = if bit == 1 { p1 } else { 65536 - p1 };
        self.bits += self.lg[(p >> 4) as usize] as f64;
        bit
    }
}
fn lg_table() -> Vec<f32> {
    (0..=4096).map(|i| -(((i as f64).max(0.5)) / 4096.0).log2() as f32).collect()
}

/// Real encode (same bytes as production `encode_stage4` for the same contexts).
fn enc_real(vs: &[u32], ctx: &[u8], k: usize) -> Vec<u8> {
    let r = rates();
    let mut models = vec![M::new(); k];
    let mut enc = BinEnc::new();
    for (&v, &c) in vs.iter().zip(ctx) {
        models[c as usize].code(&mut enc, v, r);
    }
    enc.finish()
}

fn meta_of(m: &Value, w: usize, h: usize) -> ImageMetadata {
    let Some(cfa) = m["cfa"].as_array().filter(|a| !a.is_empty()) else {
        return ImageMetadata::without_cfa(w, h, SampleLayout::Monochrome);
    };
    let dim: Vec<usize> = m["cfa_dim"].as_array().unwrap().iter().map(|v| v.as_u64().unwrap() as usize).collect();
    let org: Vec<usize> = m["cfa_origin"].as_array().unwrap().iter().map(|v| v.as_u64().unwrap() as usize).collect();
    let entries: Vec<ColorFilter> = cfa.iter().map(|v| ColorFilter::try_from(v.as_u64().unwrap() as u8).unwrap()).collect();
    let pat = CfaPattern { width: dim[1], height: dim[0], entries, origin_x: org[0], origin_y: org[1] };
    let bayer = ImageMetadata::with_cfa(w, h, SampleLayout::BayerCfa, pat.clone());
    if (dim[0], dim[1]) == (2, 2) && bayer.validate().is_ok() { bayer } else { ImageMetadata::with_cfa(w, h, SampleLayout::OtherCfa, pat) }
}

struct Loaded {
    samples: Vec<u16>,
    w: usize,
    h: usize,
    bd: u8,
    meta: ImageMetadata,
    stage2: Stage2Output,
}
fn find<'a>(manifest: &'a Value, id: &str) -> &'a Value {
    manifest.as_array().unwrap().iter().find(|m| m["id"].as_str() == Some(id)).expect("id not in manifest")
}
fn load(m: &Value) -> Loaded {
    let (w, h) = (m["width"].as_u64().unwrap() as usize, m["height"].as_u64().unwrap() as usize);
    let bd = m["bit_depth"].as_u64().unwrap() as u8;
    let bytes = std::fs::read(m["canon_path"].as_str().unwrap()).unwrap();
    let samples: Vec<u16> = bytes.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
    let meta = meta_of(m, w, h);
    let s2 = encode_stage2(&samples, w, h, bd, meta.clone()).unwrap();
    Loaded { samples, w, h, bd, meta, stage2: s2 }
}

/// Calls `f(left, above, above_left, residual)` for every residual in Stage 4 coding order, with the same causal
/// neighbors the production Stage 3 uses (Bayer planes row-major, or spatial neighbors in Morton order).
fn walk(s3: &Stage3Output, mut f: impl FnMut(Option<u32>, Option<u32>, Option<u32>, u32)) {
    match (s3.data(), s3.mode()) {
        (OrganizedResiduals::BayerPlanes(data), Some(Stage2Mode::BayerPlanes { planes, .. })) => {
            for (v, p) in data.iter().zip(planes) {
                let w = p.width;
                for i in 0..v.len() {
                    let (x, y) = (i % w, i / w);
                    f((x > 0).then(|| v[i - 1]), (y > 0).then(|| v[i - w]), (x > 0 && y > 0).then(|| v[i - w - 1]), v[i]);
                }
            }
        }
        (OrganizedResiduals::Morton(r), _) => {
            let (width, height) = (s3.metadata().width, s3.metadata().height);
            let mut seen = vec![0u32; width * height];
            let mut k = 0;
            let _ = morton_visit::<()>(width, height, |x, y| {
                let left = (x > 0).then(|| seen[y * width + x - 1]);
                let above = (y > 0).then(|| seen[(y - 1) * width + x]);
                let al = (x > 0 && y > 0).then(|| seen[(y - 1) * width + x - 1]);
                f(left, above, al, r[k]);
                seen[y * width + x] = r[k];
                k += 1;
                Ok(())
            });
        }
        _ => panic!("unexpected organization"),
    }
}

/// Windowed sample of the coding sequence: 8 windows, total 10% (same as the Deja Vu experiment).
fn sampled(g: usize, n: usize) -> bool {
    let wl = n / 80;
    let t = g * 8 / n.max(1);
    t < 8 && g >= t * (n / 8) && g < t * (n / 8) + wl
}

/// Deterministic split by sorted id (identical to `exp_lisa_pcq`): every 3rd is TEST; of the rest, every 4th is VAL.
fn split(manifest: &Value) -> (Vec<String>, Vec<String>, Vec<String>) {
    let mut ids: Vec<String> = manifest.as_array().unwrap().iter().map(|m| m["id"].as_str().unwrap().to_string()).collect();
    ids.sort();
    let (mut train, mut val, mut test, mut rest) = (vec![], vec![], vec![], vec![]);
    for (i, id) in ids.into_iter().enumerate() {
        if i % 3 == 2 { test.push(id) } else { rest.push(id) }
    }
    for (j, id) in rest.into_iter().enumerate() {
        if j % 4 == 3 { val.push(id) } else { train.push(id) }
    }
    (train, val, test)
}
/// Replays the first `l - 1` splits into `(root, nodes)` for `lisa_or128_tree::eval`.
fn build(splits: &[Value], l: usize) -> (u16, Vec<(u8, u8, u16, u16)>) {
    let mut nodes: Vec<(u8, u8, u16, u16)> = vec![];
    let mut root = 0x8000u16;
    let mut loc: Vec<Option<(usize, bool)>> = vec![None]; // None = root
    for s in splits.iter().take(l - 1) {
        let (p, q) = (s["leaf"].as_u64().unwrap() as usize, s["new_leaf"].as_u64().unwrap() as usize);
        let n = nodes.len();
        nodes.push((s["feature"].as_u64().unwrap() as u8, s["threshold"].as_u64().unwrap() as u8, 0x8000 | p as u16, 0x8000 | q as u16));
        match loc[p] {
            None => root = n as u16,
            Some((pn, hi)) => {
                if hi { nodes[pn].3 = n as u16 } else { nodes[pn].2 = n as u16 }
            }
        }
        loc[p] = Some((n, false));
        assert_eq!(loc.len(), q);
        loc.push(Some((n, true)));
    }
    (root, nodes)
}
fn main() {
    let a: Vec<String> = std::env::args().collect();
    let manifest: Value = serde_json::from_slice(&std::fs::read(&a[1]).unwrap()).unwrap();
    let sp: Value = serde_json::from_slice(&std::fs::read(&a[2]).unwrap()).unwrap();
    let splits = sp["splits"].as_array().unwrap();
    let grid = [1usize, 8, 32, 64, 128, 192];
    let trees: Vec<_> = grid.iter().map(|&l| build(splits, l)).collect();
    let (tr, _, _) = split(&manifest);
    let (mut tt, mut tg) = (0usize, vec![0usize; grid.len()]);
    let mut rows = vec![];
    for id in &tr {
        let l = load(find(&manifest, id));
        let s3 = classify_stage3_with(l.stage2, Stage3Rule::Or128).unwrap();
        let ctx: Vec<u8> = match s3.contexts() {
            OrganizedContexts::Morton(c) => c.clone(),
            OrganizedContexts::BayerPlanes(p) => p.iter().flatten().copied().collect(),
        };
        let (mut vs, mut fs) = (vec![], vec![]);
        walk(&s3, |x, y, z, v| {
            vs.push(v);
            fs.push(tree::features(x, y, z));
        });
        let tb = enc_real(&vs, &ctx, 128).len();
        tt += tb;
        let mut per = vec![];
        for (gi, (root, nodes)) in trees.iter().enumerate() {
            let c: Vec<u8> = fs.iter().map(|f| tree::eval(*root, nodes, f)).collect();
            let b = enc_real(&vs, &c, grid[gi]).len();
            tg[gi] += b;
            per.push(b);
        }
        println!("{id}: table {tb} | {}", grid.iter().zip(&per).map(|(l, b)| format!("L{l}={:+.3}%", (*b as f64 / tb as f64 - 1.0) * 100.0)).collect::<Vec<_>>().join(" "));
        rows.push(json!({"id": id, "table_bytes": tb, "tree_bytes": per}));
    }
    println!("TOTAL table {tt} B");
    for (i, &l) in grid.iter().enumerate() {
        println!("L={l:3}: {} B ({:+.3}% vs table; {:+.3}% vs L1)", tg[i], (tg[i] as f64 / tt as f64 - 1.0) * 100.0, (tg[i] as f64 / tg[0] as f64 - 1.0) * 100.0);
    }
    std::fs::write(&a[3], serde_json::to_vec_pretty(&json!({"grid": grid, "table_total": tt, "tree_totals": tg, "per_image": rows})).unwrap()).unwrap();
}
