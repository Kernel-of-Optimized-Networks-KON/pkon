//! LISA Deja Vu Tree experiment: greedy merging of the 128 OR-128 contexts into fewer Stage 4 model groups.
//! Isolated: uses only the public `lisa` API. Stage 4 coding is re-implemented locally (same binarization, same PKON
//! counted estimator, same BinEnc/BinDec) and is gated by byte-equality with production `encode_stage4` at identity map.
//! Merge cost = adaptive code length (-log2 p of the actual coder probabilities) over the original observation order,
//! measured on a windowed sample (10% of each image in 8 contiguous windows); final sizes are REAL range-coder bytes.
//! Usage: lisa_dejavu <manifest.json> <out.json> <id> [<id>...]
use nanoscope::lisa::*;
use nanoscope::lisa_bayer_plane::{decode_stage1, encode_stage2, Stage1Mode};
use nanoscope::lisa_or128 as or128;
use nanoscope::rangecoder::{BinDec, BinEnc, BitIo};
use serde_json::{json, Value};
use std::time::Instant;

const E_MAX: usize = 33;
const LIMIT: usize = 255;
const NG: usize = 128;

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
    fn code<I: BitIo>(&mut self, io: &mut I, v: u32, r: &[u32; LIMIT + 1]) -> u32 {
        let x = u64::from(v) + 1;
        let e_in = (64 - x.leading_zeros()) as usize;
        let mut e = E_MAX;
        for i in 1..E_MAX {
            if Self::bit(io, &mut self.unary[i], (e_in > i) as u32, r) == 0 {
                e = i;
                break;
            }
        }
        let mut out = 1u64;
        for j in (0..e - 1).rev() {
            let want = ((x >> j) & 1) as u32;
            let b = if j == e - 2 { Self::bit(io, &mut self.mant[e], want, r) } else { io.code(32768, want) };
            out = (out << 1) | u64::from(b);
        }
        (out - 1) as u32
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

/// Real encode: Bayer planes in order, group = map[ctx].
fn enc_real(res: &[Vec<u32>; 4], ctx: &[Vec<u8>; 4], map: &[u8; NG], k: usize) -> Vec<u8> {
    let r = rates();
    let mut models = vec![M::new(); k];
    let mut enc = BinEnc::new();
    for (rv, cv) in res.iter().zip(ctx) {
        for (&v, &c) in rv.iter().zip(cv) {
            models[map[c as usize] as usize].code(&mut enc, v, r);
        }
    }
    enc.finish()
}

/// Real decode: contexts recomputed causally with OR-128, then mapped. Returns residual planes.
fn dec_real(payload: &[u8], dims: &[(usize, usize); 4], map: &[u8; NG], k: usize) -> ([Vec<u32>; 4], usize) {
    let r = rates();
    let mut models = vec![M::new(); k];
    let mut dec = BinDec::new(payload);
    let mut out: [Vec<u32>; 4] = Default::default();
    for (pl, &(w, h)) in out.iter_mut().zip(dims) {
        pl.reserve_exact(w * h);
        for i in 0..w * h {
            let (x, y) = (i % w, i / w);
            let l = (x > 0).then(|| pl[i - 1]);
            let a = (y > 0).then(|| pl[i - w]);
            let al = (x > 0 && y > 0).then(|| pl[i - w - 1]);
            let g = map[or128::context(l, a, al) as usize] as usize;
            let v = models[g].code(&mut dec, 0, r);
            pl.push(v);
        }
    }
    (out, dec.consumed())
}

type Stream = Vec<(u32, u32)>; // (global coding index, zigzag residual)

fn cost(s: &Stream, lg: &[f32]) -> f64 {
    let r = rates();
    let mut m = M::new();
    let mut io = CostIo { bits: 0.0, lg };
    for &(_, v) in s {
        m.code(&mut io, v, r);
    }
    io.bits
}
fn merge(a: &Stream, b: &Stream) -> Stream {
    let (mut o, mut i, mut j) = (Vec::with_capacity(a.len() + b.len()), 0, 0);
    while i < a.len() || j < b.len() {
        if j == b.len() || (i < a.len() && a[i].0 < b[j].0) {
            o.push(a[i]);
            i += 1;
        } else {
            o.push(b[j]);
            j += 1;
        }
    }
    o
}

struct Node {
    s: Vec<Stream>, // per training image
    c: f64,
}
fn node_cost(s: &[Stream], lg: &[f32]) -> f64 {
    s.iter().map(|x| cost(x, lg)).sum()
}
fn delta(a: &Node, b: &Node, lg: &[f32]) -> (f64, Vec<Stream>) {
    let m: Vec<Stream> = a.s.iter().zip(&b.s).map(|(x, y)| merge(x, y)).collect();
    let c = node_cost(&m, lg);
    (c - a.c - b.c, m)
}

/// Windowed sample of the coding sequence: 8 windows, total 10%.
fn sampled(g: usize, n: usize) -> bool {
    let wl = n / 80;
    let t = g * 8 / n.max(1);
    t < 8 && g >= t * (n / 8) && g < t * (n / 8) + wl
}

fn streams_of(ctx: &[Vec<u8>; 4], res: &[Vec<u32>; 4]) -> Vec<Stream> {
    let n: usize = res.iter().map(|r| r.len()).sum();
    let mut s: Vec<Stream> = vec![vec![]; NG];
    let mut g = 0usize;
    for (rv, cv) in res.iter().zip(ctx) {
        for (&v, &c) in rv.iter().zip(cv) {
            if sampled(g, n) {
                s[c as usize].push((g as u32, v));
            }
            g += 1;
        }
    }
    s
}

struct Tree {
    merges: Vec<(usize, usize)>, // (a, b) node ids; new node id = NG + index
    est_bits: Vec<f64>,          // est_bits[k] = sampled total bits at k groups (k = 1..=NG)
    secs: f64,
}

fn build_tree(train: &[Vec<Stream>], lg: &[f32]) -> Tree {
    let t0 = Instant::now();
    let mut nodes: Vec<Option<Node>> = (0..NG)
        .map(|g| {
            let s: Vec<Stream> = train.iter().map(|im| im[g].clone()).collect();
            let c = node_cost(&s, lg);
            Some(Node { s, c })
        })
        .collect();
    let mut active: Vec<usize> = (0..NG).collect();
    let total = |nodes: &Vec<Option<Node>>, act: &[usize]| act.iter().map(|&i| nodes[i].as_ref().unwrap().c).sum::<f64>();
    let mut est = vec![0.0; NG + 1];
    est[NG] = total(&nodes, &active);
    let mut dm = std::collections::HashMap::<(usize, usize), f64>::new();
    let pairs: Vec<(usize, usize)> = (0..NG).flat_map(|a| (a + 1..NG).map(move |b| (a, b))).collect();
    let eval = |pairs: &[(usize, usize)], nodes: &Vec<Option<Node>>| -> Vec<f64> {
        let th = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4);
        let chunk = pairs.len().div_ceil(th).max(1);
        let mut out = vec![0.0; pairs.len()];
        std::thread::scope(|sc| {
            for (pc, oc) in pairs.chunks(chunk).zip(out.chunks_mut(chunk)) {
                sc.spawn(move || {
                    for (p, o) in pc.iter().zip(oc.iter_mut()) {
                        *o = delta(nodes[p.0].as_ref().unwrap(), nodes[p.1].as_ref().unwrap(), lg).0;
                    }
                });
            }
        });
        out
    };
    for (p, d) in pairs.iter().zip(eval(&pairs, &nodes)) {
        dm.insert(*p, d);
    }
    let mut merges = vec![];
    while active.len() > 1 {
        let (&(a, b), _) = dm.iter().min_by(|x, y| x.1.partial_cmp(y.1).unwrap().then(x.0.cmp(y.0))).unwrap();
        let (_, ms) = delta(nodes[a].as_ref().unwrap(), nodes[b].as_ref().unwrap(), lg);
        let c = node_cost(&ms, lg);
        let id = nodes.len();
        nodes[a] = None;
        nodes[b] = None;
        nodes.push(Some(Node { s: ms, c }));
        active.retain(|&x| x != a && x != b);
        dm.retain(|k, _| k.0 != a && k.0 != b && k.1 != a && k.1 != b);
        let np: Vec<(usize, usize)> = active.iter().map(|&o| (o, id)).collect();
        for (p, d) in np.iter().zip(eval(&np, &nodes)) {
            dm.insert(*p, d);
        }
        active.push(id);
        merges.push((a, b));
        est[active.len()] = total(&nodes, &active);
    }
    Tree { merges, est_bits: est, secs: t0.elapsed().as_secs_f64() }
}

/// Context -> group map after applying the first `NG - k` merges; groups relabelled 0..k by first appearance.
fn cut(tree: &Tree, k: usize) -> [u8; NG] {
    let mut parent: Vec<usize> = (0..NG + tree.merges.len()).collect();
    fn root(p: &mut [usize], mut x: usize) -> usize {
        while p[x] != x {
            p[x] = p[p[x]];
            x = p[x];
        }
        x
    }
    for (i, &(a, b)) in tree.merges.iter().take(NG - k).enumerate() {
        parent[a] = NG + i;
        parent[b] = NG + i;
    }
    let mut label = std::collections::HashMap::new();
    let mut map = [0u8; NG];
    for c in 0..NG {
        let r = root(&mut parent, c);
        let n = label.len();
        map[c] = *label.entry(r).or_insert(n) as u8;
    }
    assert_eq!(label.len(), k);
    map
}
/// Experimental-only table: one fixed-width group id per original context.
fn meta_bytes(k: usize) -> usize {
    if k == 1 {
        return 0;
    }
    (NG * (usize::BITS - (k - 1).leading_zeros()) as usize).div_ceil(8)
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

struct Img {
    id: String,
    w: usize,
    h: usize,
    bd: u8,
    samples: Vec<u16>,
    meta: ImageMetadata,
    res: [Vec<u32>; 4],
    ctx: [Vec<u8>; 4],
    dims: [(usize, usize); 4],
    prod_bytes: usize,
    prod_payload: Vec<u8>,
    prod_enc_best: f64,
    prod_dec_best: f64,
}

/// Full lossless check: decode with the Deja Vu map, un-plane, run the production Stage 1 inverse, compare to source.
fn roundtrip(im: &Img, payload: &[u8], map: &[u8; NG], k: usize) -> (bool, f64) {
    let t = Instant::now();
    let (planes, consumed) = dec_real(payload, &im.dims, map, k);
    let dec_s = t.elapsed().as_secs_f64();
    let mut residuals = Vec::with_capacity(im.w * im.h);
    let _ = morton_visit::<()>(im.w, im.h, |x, y| {
        let pi = (y & 1) * 2 + (x & 1);
        residuals.push(planes[pi][(y >> 1) * im.dims[pi].0 + (x >> 1)]);
        Ok(())
    });
    let ok = planes == im.res && consumed == payload.len() && {
        let s1 = Stage1Output { residuals, metadata: im.meta.clone(), bit_depth: im.bd };
        decode_stage1(&s1, Stage1Mode::default()).map(|d| d == im.samples).unwrap_or(false)
    };
    (ok, dec_s)
}

fn best3(f: &mut dyn FnMut()) -> f64 {
    (0..3).map(|_| { let t = Instant::now(); f(); t.elapsed().as_secs_f64() }).fold(f64::MAX, f64::min)
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let manifest: Value = serde_json::from_slice(&std::fs::read(&args[1]).unwrap()).unwrap();
    let lg = lg_table();
    let mut imgs: Vec<Img> = vec![];
    for id in &args[3..] {
        let m = manifest.as_array().unwrap().iter().find(|m| m["id"].as_str() == Some(id)).expect("id");
        let (w, h) = (m["width"].as_u64().unwrap() as usize, m["height"].as_u64().unwrap() as usize);
        let bd = m["bit_depth"].as_u64().unwrap() as u8;
        let bytes = std::fs::read(m["canon_path"].as_str().unwrap()).unwrap();
        let samples: Vec<u16> = bytes.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
        let meta = meta_of(m, w, h);
        let s2 = encode_stage2(&samples, w, h, bd, meta.clone()).unwrap();
        let s3 = classify_stage3(s2).unwrap();
        let s4 = encode_stage4(s3.clone()).unwrap();
        let (OrganizedResiduals::BayerPlanes(res), OrganizedContexts::BayerPlanes(ctx)) = (s3.data().clone(), s3.contexts().clone()) else {
            panic!("{id}: not a Bayer-plane image")
        };
        let Some(Stage2Mode::BayerPlanes { planes, .. }) = s3.mode() else { panic!() };
        let dims = [0, 1, 2, 3].map(|i| (planes[i].width, planes[i].height));
        let prod_enc_best = best3(&mut || { std::hint::black_box(encode_stage4(s3.clone()).unwrap()); });
        let prod_dec_best = best3(&mut || { std::hint::black_box(decode_stage4(&s4).unwrap()); });
        println!("loaded {id}: production OR-128 {} B", s4.payload().len());
        imgs.push(Img { id: id.clone(), w, h, bd, samples, meta, res, ctx, dims, prod_bytes: s4.payload().len(),
                        prod_payload: s4.payload().to_vec(), prod_enc_best, prod_dec_best });
    }
    let ident: [u8; NG] = std::array::from_fn(|i| i as u8);

    // Gate: local coder at identity map must reproduce production bytes exactly.
    let gate = imgs.iter().all(|im| enc_real(&im.res, &im.ctx, &ident, NG) == im.prod_payload);
    println!("GATE local coder == production payload (identity map): {gate}");
    assert!(gate);

    let streams: Vec<Vec<Stream>> = imgs.iter().map(|im| streams_of(&im.ctx, &im.res)).collect();
    let n_img = imgs.len();
    let ks: Vec<usize> = vec![128, 112, 96, 80, 64, 56, 48, 40, 32, 28, 24, 20, 16, 12, 8, 6, 4, 2, 1];

    // Leave-one-image-out: tree trained on the other images, K chosen from training cost, evaluated on the held-out image.
    let mut loo = vec![];
    for t in 0..n_img {
        let train: Vec<Vec<Stream>> = (0..n_img).filter(|&i| i != t).map(|i| streams[i].clone()).collect();
        let tree = build_tree(&train, &lg);
        let kbest = (1..=NG).min_by(|&a, &b| tree.est_bits[a].partial_cmp(&tree.est_bits[b]).unwrap()).unwrap();
        let map = cut(&tree, kbest);
        let bytes = enc_real(&imgs[t].res, &imgs[t].ctx, &map, kbest).len();
        println!("LOO hold-out {}: tree {:.1}s, K(train-est)={} -> {} B vs base {} B ({:+.3}%)", imgs[t].id, tree.secs, kbest, bytes,
                 imgs[t].prod_bytes, (bytes as f64 / imgs[t].prod_bytes as f64 - 1.0) * 100.0);
        loo.push(json!({"held_out": imgs[t].id, "tree_secs": tree.secs, "k": kbest, "bytes": bytes, "baseline_bytes": imgs[t].prod_bytes}));
    }

    // In-sample tree on all images; real bytes over a K grid.
    let tree = build_tree(&streams, &lg);
    println!("full tree built in {:.1}s", tree.secs);
    let mut grid = vec![];
    let mut best: (usize, usize) = (NG, usize::MAX);
    for &k in &ks {
        let map = cut(&tree, k);
        let per: Vec<usize> = imgs.iter().map(|im| enc_real(&im.res, &im.ctx, &map, k).len()).collect();
        let tot: usize = per.iter().sum();
        println!("K={k:3}: real total {tot} B  (+meta {} B/stream)  per-image {:?}", meta_bytes(k), per);
        if tot + meta_bytes(k) * n_img < best.1 { best = (k, tot + meta_bytes(k) * n_img); }
        grid.push(json!({"k": k, "per_image_bytes": per, "total_bytes": tot, "meta_bytes_per_stream": meta_bytes(k),
                         "est_sampled_bits": tree.est_bits[k]}));
    }
    let kb = best.0;
    let map = cut(&tree, kb);
    let base_total: usize = imgs.iter().map(|i| i.prod_bytes).sum();
    let mut rows = vec![];
    let mut all_exact = true;
    let (mut dj_tot, mut deja_tot) = (0usize, 0usize);
    for im in &imgs {
        let p1 = enc_real(&im.res, &im.ctx, &map, kb);
        let p2 = enc_real(&im.res, &im.ctx, &map, kb);
        let det = p1 == p2;
        let enc_best = best3(&mut || { std::hint::black_box(enc_real(&im.res, &im.ctx, &map, kb)); });
        let (ok, _) = roundtrip(im, &p1, &map, kb);
        let mut dec_best = f64::MAX;
        for _ in 0..3 { dec_best = dec_best.min(roundtrip(im, &p1, &map, kb).1); }
        all_exact &= ok && det;
        let with_meta = p1.len() + meta_bytes(kb);
        dj_tot += p1.len();
        deja_tot += with_meta;
        let raw = im.w * im.h * 2;
        println!("{}: base {} B -> dejavu K={kb} {} B (+{} meta) {:+.3}% | ratio raw/base {:.4} raw/dj {:.4} | S4 enc base {:.3}s dj {:.3}s | S4 dec base {:.3}s dj {:.3}s | exact={ok} det={det}",
                 im.id, im.prod_bytes, p1.len(), meta_bytes(kb), (with_meta as f64 / im.prod_bytes as f64 - 1.0) * 100.0,
                 raw as f64 / im.prod_bytes as f64, raw as f64 / with_meta as f64, im.prod_enc_best, enc_best, im.prod_dec_best, dec_best);
        rows.push(json!({"id": im.id, "raw_bytes": raw, "baseline_bytes": im.prod_bytes, "dejavu_payload_bytes": p1.len(),
            "dejavu_meta_bytes": meta_bytes(kb), "dejavu_total_bytes": with_meta, "pct_change_incl_meta": (with_meta as f64 / im.prod_bytes as f64 - 1.0) * 100.0,
            "stage4_encode_best3_baseline_s": im.prod_enc_best, "stage4_encode_best3_dejavu_s": enc_best,
            "stage4_decode_best3_baseline_s": im.prod_dec_best, "stage4_decode_best3_dejavu_s": dec_best,
            "pixels_exact_and_residuals_equal_and_payload_consumed": ok, "deterministic": det}));
    }
    let loo_tot: usize = loo.iter().map(|l| l["bytes"].as_u64().unwrap() as usize).sum();
    println!("AGG baseline {base_total} B | in-sample dejavu K={kb}: payload {dj_tot} B, +meta {deja_tot} B ({:+.4}% incl meta, {:+.4}% payload only) | LOO total {loo_tot} B ({:+.4}%, no meta)",
             (deja_tot as f64 / base_total as f64 - 1.0) * 100.0, (dj_tot as f64 / base_total as f64 - 1.0) * 100.0,
             (loo_tot as f64 / base_total as f64 - 1.0) * 100.0);
    println!("ALL_EXACT={all_exact}");
    let out = json!({"baseline_total_bytes": base_total, "selected_k": kb, "tree_build_secs_full": tree.secs, "dejavu_payload_total": dj_tot,
        "dejavu_total_incl_meta": deja_tot, "loo": loo, "loo_total_bytes": loo_tot, "k_grid_real_bytes": grid, "per_image": rows,
        "merge_order": tree.merges, "map_at_selected_k": map.to_vec(), "all_exact": all_exact,
        "notes": "merge cost = adaptive code length on 10% windowed sample; final bytes are real range-coder output; K chosen in-sample on real bytes (optimistic); LOO is the held-out estimate"});
    std::fs::write(&args[2], serde_json::to_vec_pretty(&out).unwrap()).unwrap();
}
