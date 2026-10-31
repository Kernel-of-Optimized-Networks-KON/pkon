//! OR-128 context tree experiment: greedy top-down learning of the Stage 3 classifier from a single leaf.
//! Isolated: uses only the public `lisa` API plus `lisa_or128_tree::{features, eval}`. Stage 4 is re-implemented locally
//! (same binarization, same counted estimator, same BinEnc) and gated by byte-equality with production `encode_stage4`.
//!
//! Split objective: for a leaf, a candidate (feature <= threshold) is scored by the ADAPTIVE code length (-log2 p of the
//! actual Stage 4 model probabilities, models fresh per image) of the two child subsequences, in original coding order, on a
//! windowed 10% sample of each TRAIN image. Gain = cost(leaf) - cost(lo) - cost(hi). Growth is best-first (largest gain), so
//! the first L-1 splits form the L-leaf tree. Final numbers are real range-coder bytes.
//!
//!   exp_or128_tree split  <manifest.json>
//!   exp_or128_tree train  <manifest.json> <out_splits.json> [max_leaves=255] [min_child=4000]
//!   exp_or128_tree val    <manifest.json> <splits.json> <out.json>
//!   exp_or128_tree gen    <splits.json> <L> <out.rs>
//!   exp_or128_tree final  <manifest.json> <out.json> <id>...         (production path, Or128 vs Or128Tree)
use nanoscope::lisa::*;
use nanoscope::lisa_bayer_plane::{decode_stage1, encode_stage2, restore_stage1_with_mode};
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

// ------------------------------------------------------------------------------------------------ training
#[derive(Clone, Copy)]
struct S {
    f: [u8; NF],
    v: u32,
}
struct Leaf {
    id: usize,
    s: Vec<Vec<S>>, // per training image, original coding order
    cost: f64,
    best: Option<(f64, u8, u8, f64, f64)>, // gain, feature, threshold, cost_lo, cost_hi
}
fn stream_cost(streams: &[Vec<S>], lg: &[f32], pred: impl Fn(&S) -> bool) -> f64 {
    let r = rates();
    let mut io = CostIo { bits: 0.0, lg };
    for st in streams {
        let mut m = M::new();
        for s in st.iter().filter(|s| pred(s)) {
            m.code(&mut io, s.v, r);
        }
    }
    io.bits
}
fn find_best(leaf: &mut Leaf, lg: &[f32], min_child: usize) {
    // Candidate thresholds: observed feature values with both sides at least `min_child` samples.
    let mut hist = vec![[0usize; 32]; NF];
    for st in &leaf.s {
        for s in st {
            for (f, h) in hist.iter_mut().enumerate() {
                h[s.f[f] as usize] += 1;
            }
        }
    }
    let total: usize = leaf.s.iter().map(|s| s.len()).sum();
    let mut cands = vec![];
    for (f, h) in hist.iter().enumerate() {
        let mut lo = 0;
        for (t, &c) in h.iter().enumerate() {
            lo += c;
            if c > 0 && lo >= min_child && total - lo >= min_child {
                cands.push((f as u8, t as u8));
            }
        }
    }
    let next = AtomicUsize::new(0);
    let th = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4);
    let mut res: Vec<(u8, u8, f64, f64)> = std::thread::scope(|sc| {
        let hs: Vec<_> = (0..th)
            .map(|_| {
                sc.spawn(|| {
                    let r = rates();
                    let mut out = vec![];
                    loop {
                        let i = next.fetch_add(1, Ordering::Relaxed);
                        let Some(&(f, t)) = cands.get(i) else { break };
                        let (mut lo_io, mut hi_io) = (CostIo { bits: 0.0, lg }, CostIo { bits: 0.0, lg });
                        for st in &leaf.s {
                            let (mut ml, mut mh) = (M::new(), M::new());
                            for s in st {
                                if s.f[f as usize] <= t { ml.code(&mut lo_io, s.v, r) } else { mh.code(&mut hi_io, s.v, r) }
                            }
                        }
                        out.push((f, t, lo_io.bits, hi_io.bits));
                    }
                    out
                })
            })
            .collect();
        hs.into_iter().flat_map(|h| h.join().unwrap()).collect()
    });
    res.sort_by(|a, b| (a.0, a.1).cmp(&(b.0, b.1)));
    leaf.best = None;
    for (f, t, cl, ch) in res {
        let gain = leaf.cost - cl - ch;
        if leaf.best.map_or(true, |b| gain > b.0) {
            leaf.best = Some((gain, f, t, cl, ch));
        }
    }
}

fn train(manifest: &Value, out: &str, max_leaves: usize, min_child: usize) {
    let t0 = Instant::now();
    let lg = lg_table();
    let (tr, _, _) = split(manifest);
    let mut streams: Vec<Vec<S>> = vec![];
    for id in &tr {
        let l = load(find(manifest, id));
        let s3 = classify_stage3_with(l.stage2, Stage3Rule::Or128).unwrap();
        let n = l.w * l.h;
        let (mut g, mut st) = (0usize, vec![]);
        walk(&s3, |a, b, c, v| {
            if sampled(g, n) {
                st.push(S { f: tree::features(a, b, c), v });
            }
            g += 1;
        });
        println!("loaded {id}: {} sampled of {n}", st.len());
        streams.push(st);
    }
    let load_secs = t0.elapsed().as_secs_f64();
    let t1 = Instant::now();
    let c0 = stream_cost(&streams, &lg, |_| true);
    let mut leaves = vec![Leaf { id: 0, s: streams, cost: c0, best: None }];
    find_best(&mut leaves[0], &lg, min_child);
    let mut steps = vec![];
    while leaves.len() < max_leaves {
        let Some((bi, _)) = leaves
            .iter()
            .enumerate()
            .filter_map(|(i, l)| l.best.filter(|b| b.0 > 64.0).map(|b| (i, b.0)))
            .max_by(|a, b| a.1.partial_cmp(&b.1).unwrap().then(b.0.cmp(&a.0)))
        else {
            println!("no positive-gain split left");
            break;
        };
        let leaf = leaves.swap_remove(bi);
        let (gain, f, t, cl, ch) = leaf.best.unwrap();
        let new_id = leaves.len() + 1;
        let (mut lo, mut hi) = (vec![], vec![]);
        for st in &leaf.s {
            let (a, b): (Vec<S>, Vec<S>) = st.iter().partition(|s| s.f[f as usize] <= t);
            lo.push(a);
            hi.push(b);
        }
        let mut a = Leaf { id: leaf.id, s: lo, cost: cl, best: None };
        let mut b = Leaf { id: new_id, s: hi, cost: ch, best: None };
        drop(leaf.s);
        find_best(&mut a, &lg, min_child);
        find_best(&mut b, &lg, min_child);
        leaves.push(a);
        leaves.push(b);
        let total: f64 = leaves.iter().map(|l| l.cost).sum();
        println!("L={:3} split leaf {} on {} <= {} gain {:.0} bits; est total {:.0} bits; {:.0}s", leaves.len(), leaf.id, FEATURE_NAMES[f as usize], t, gain, total, t1.elapsed().as_secs_f64());
        steps.push(json!({"leaf": leaf.id, "new_leaf": new_id, "feature": f, "feature_name": FEATURE_NAMES[f as usize], "threshold": t,
                          "gain_bits": gain, "leaves_after": leaves.len(), "est_total_bits": total}));
    }
    let o = json!({"train_ids": tr, "min_child": min_child, "max_leaves": max_leaves, "est_cost_single_leaf_bits": c0,
                   "load_secs": load_secs, "greedy_secs": t1.elapsed().as_secs_f64(), "feature_names": FEATURE_NAMES, "splits": steps});
    std::fs::write(out, serde_json::to_vec_pretty(&o).unwrap()).unwrap();
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

fn ctx_flat(s3: &Stage3Output) -> Vec<u8> {
    match s3.contexts() {
        OrganizedContexts::Morton(c) => c.clone(),
        OrganizedContexts::BayerPlanes(p) => p.iter().flatten().copied().collect(),
    }
}

const GRID: [usize; 14] = [1, 2, 4, 8, 16, 32, 48, 64, 96, 128, 160, 192, 224, 255];

fn val(manifest: &Value, splits_path: &str, out: &str) {
    let sp: Value = serde_json::from_slice(&std::fs::read(splits_path).unwrap()).unwrap();
    let splits = sp["splits"].as_array().unwrap();
    let (_, va, _) = split(manifest);
    let grid: Vec<usize> = GRID.iter().copied().filter(|&l| l <= splits.len() + 1).collect();
    let trees: Vec<_> = grid.iter().map(|&l| build(splits, l)).collect();
    let mut per: Vec<Vec<usize>> = vec![vec![]; grid.len()];
    let (mut base_bytes, mut gate) = (vec![], true);
    for id in &va {
        let l = load(find(manifest, id));
        let s3 = classify_stage3_with(l.stage2.clone(), Stage3Rule::Or128).unwrap();
        let prod = encode_stage4(s3.clone()).unwrap();
        let mut vs = vec![];
        let mut fs: Vec<[u8; NF]> = vec![];
        walk(&s3, |a, b, c, v| {
            vs.push(v);
            fs.push(tree::features(a, b, c));
        });
        gate &= enc_real(&vs, &ctx_flat(&s3), 128) == prod.payload();
        base_bytes.push(prod.payload().len());
        for (gi, (root, nodes)) in trees.iter().enumerate() {
            let ctx: Vec<u8> = fs.iter().map(|f| tree::eval(*root, nodes, f)).collect();
            per[gi].push(enc_real(&vs, &ctx, grid[gi]).len());
        }
        println!("val {id}: base {} B | {}", prod.payload().len(), grid.iter().zip(&per).map(|(l, p)| format!("L{l}={}", p.last().unwrap())).collect::<Vec<_>>().join(" "));
    }
    println!("GATE local coder == production payload at OR-128 table: {gate}");
    assert!(gate);
    let base: usize = base_bytes.iter().sum();
    let tot: Vec<usize> = per.iter().map(|p| p.iter().sum()).collect();
    let best = *tot.iter().min().unwrap();
    // Declared before running: smallest L within 0.05% of the best validation total.
    let sel = (0..grid.len()).find(|&i| tot[i] as f64 <= best as f64 * 1.0005).unwrap();
    for (i, &l) in grid.iter().enumerate() {
        println!("L={l:3}: val total {} B ({:+.3}% vs OR-128 table {base} B){}", tot[i], (tot[i] as f64 / base as f64 - 1.0) * 100.0, if i == sel { "  <== selected" } else { "" });
    }
    let o = json!({"val_ids": va, "baseline_bytes": base_bytes, "baseline_total": base, "grid": grid, "per_image": per, "totals": tot,
                   "selection_rule": "smallest L within 0.05% of the best validation total", "selected_leaves": grid[sel], "gate_local_eq_production": gate});
    std::fs::write(out, serde_json::to_vec_pretty(&o).unwrap()).unwrap();
}

fn gen(splits_path: &str, l: usize, out: &str) {
    let sp: Value = serde_json::from_slice(&std::fs::read(splits_path).unwrap()).unwrap();
    let (root, nodes) = build(sp["splits"].as_array().unwrap(), l);
    let mut s = format!("//! GENERATED (research): frozen OR-128 context tree, {l} leaves, trained by `exp_or128_tree train` on the 13 TRAIN images only\n//! (split of `exp_lisa_pcq`), complexity chosen on the 4 VAL images. Do not edit by hand.\n");
    s += &format!("pub const OR128_TREE_LEAVES: usize = {l};\n/// Reference to the root: a leaf id when the high bit is set, otherwise an index into `OR128_TREE_NODES`.\npub const OR128_TREE_ROOT: u16 = {root:#06x};\n");
    s += "/// `(feature, threshold, lo, hi)`: go `lo` when `features[feature] <= threshold`, otherwise `hi`; children are references as above.\npub const OR128_TREE_NODES: &[(u8, u8, u16, u16)] = &[\n";
    for (f, t, lo, hi) in &nodes {
        s += &format!("    ({f}, {t}, {lo:#06x}, {hi:#06x}),\n");
    }
    s += "];\n";
    std::fs::write(out, s).unwrap();
}

// ------------------------------------------------------------------------------------------------ final (production path)
fn best3(f: &mut dyn FnMut()) -> f64 {
    (0..3).map(|_| { let t = Instant::now(); f(); t.elapsed().as_secs_f64() }).fold(f64::MAX, f64::min)
}
fn final_eval(manifest: &Value, out: &str, ids: &[String]) {
    let mut rows = vec![];
    let mut all_ok = true;
    for id in ids {
        let l = load(find(manifest, id));
        let mut r = serde_json::Map::new();
        r.insert("id".into(), json!(id));
        for (name, rule) in [("or128_table", Stage3Rule::Or128), ("or128_tree", Stage3Rule::Or128Tree)] {
            let t = Instant::now();
            let s3 = classify_stage3_with(l.stage2.clone(), rule).unwrap();
            let s4 = encode_stage4(s3.clone()).unwrap();
            let enc_s = t.elapsed().as_secs_f64();
            let s4b = encode_stage4(classify_stage3_with(l.stage2.clone(), rule).unwrap()).unwrap();
            let det = s4 == s4b;
            let d3 = decode_stage4(&s4).unwrap(); // enforces full payload consumption
            let ctx_resid_eq = d3 == s3;
            let (s1b, mode) = restore_stage1_with_mode(&d3.into_stage2()).unwrap();
            let exact = decode_stage1(&s1b, mode).unwrap() == l.samples && s1b.metadata == l.meta;
            let rec_ok = s3.rule() == Some(rule) && !matches!(s4.history().last(), None);
            let s3_t = best3(&mut || { std::hint::black_box(classify_stage3_with(l.stage2.clone(), rule).unwrap()); });
            let s4e_t = best3(&mut || { std::hint::black_box(encode_stage4(s3.clone()).unwrap()); });
            let s4d_t = best3(&mut || { std::hint::black_box(decode_stage4(&s4).unwrap()); });
            let ok = det && ctx_resid_eq && exact && rec_ok;
            all_ok &= ok;
            println!("{id} {name}: {} B | exact={exact} det={det} ctx+resid_eq={ctx_resid_eq} | first enc {enc_s:.2}s | best3 stage3 {s3_t:.3}s s4enc {s4e_t:.3}s s4dec {s4d_t:.3}s", s4.payload().len());
            r.insert(name.into(), json!({"payload_bytes": s4.payload().len(), "exact": exact, "deterministic": det, "decoded_stage3_equals_encoder_stage3": ctx_resid_eq,
                "first_encode_s": enc_s, "stage3_best3_s": s3_t, "stage4_encode_best3_s": s4e_t, "stage4_decode_best3_s": s4d_t, "ok": ok}));
        }
        r.insert("raw_bytes".into(), json!(l.w * l.h * 2));
        let _ = l.bd;
        rows.push(Value::Object(r));
    }
    println!("ALL_OK={all_ok}");
    std::fs::write(out, serde_json::to_vec_pretty(&rows).unwrap()).unwrap();
    if !all_ok {
        std::process::exit(1);
    }
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let man = |p: &str| -> Value { serde_json::from_slice(&std::fs::read(p).unwrap()).unwrap() };
    match a[1].as_str() {
        "split" => {
            let (tr, va, te) = split(&man(&a[2]));
            println!("{}", serde_json::to_string_pretty(&json!({"train": tr, "val": va, "test": te})).unwrap());
        }
        "train" => train(&man(&a[2]), &a[3], a.get(4).map_or(255, |s| s.parse().unwrap()), a.get(5).map_or(4000, |s| s.parse().unwrap())),
        "val" => val(&man(&a[2]), &a[3], &a[4]),
        "gen" => gen(&a[2], a[3].parse().unwrap(), &a[4]),
        "final" => final_eval(&man(&a[2]), &a[3], &a[4..]),
        _ => panic!("unknown subcommand"),
    }
}
