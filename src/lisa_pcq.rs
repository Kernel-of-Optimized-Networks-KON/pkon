//! EXPERIMENT (isolated): Predictive Context Quantizer (PCQ) for LISA Stage 3.
//!
//! Replaces only the Stage 3 context assignment. Stages 1, 2 and the Stage 4 binarization, `Bit` estimator and range
//! coder are the production ones (the model code below is a line-for-line twin of `S4Model::code` in lisa.rs; the
//! baseline classifier run through this path must reproduce `encode_stage4` bytes, which the runner checks).
//! Nothing here is used by production LISA.
//!
//! Feature: each causal neighbor (W, N, NW zigzag residual) is mapped to a half-octave level (`level`); a missing
//! neighbor is imputed with the rounded mean of the available ones (all-missing => 0). The ordered level triple is the
//! pattern key. An offline-trained, frozen table maps every key to a context id (see `agglomerate`).
use crate::lisa::*;
use crate::rangecoder::{BinDec, BinEnc, Bit, BitIo};

pub const LEVELS: usize = 22;
pub const KEYS: usize = LEVELS * LEVELS * LEVELS;
/// Target bins for statistics: bit length e of (zigzag + 1), 1..=18 stored at e-1 (longer values clamp into the last).
pub const E_BINS: usize = 18;
const E_MAX: usize = 33; // = S4_E_MAX in lisa.rs

/// Half-octave magnitude level: 0,1,2,3 exact; then {4-5, 6-7, 8-11, 12-15, 16-23, ...}; capped at LEVELS-1.
#[inline]
pub fn level(v: u32) -> usize {
    if v < 4 {
        return v as usize;
    }
    let l = 31 - v.leading_zeros() as usize;
    let half = ((v >> (l - 1)) & 1) as usize;
    (4 + 2 * (l - 2) + half).min(LEVELS - 1)
}

/// Pattern key of the causal neighborhood (`None` = outside the plane). Deterministic for every input.
#[inline]
pub fn key(left: Option<u32>, above: Option<u32>, above_left: Option<u32>) -> usize {
    let nb = [left, above, above_left];
    let (mut sum, mut n) = (0u64, 0u64);
    for v in nb.into_iter().flatten() {
        sum += u64::from(v);
        n += 1;
    }
    let fill = if n == 0 { 0 } else { ((sum + n / 2) / n) as u32 };
    let [a, b, c] = nb.map(|v| level(v.unwrap_or(fill)));
    (a * LEVELS + b) * LEVELS + c
}

#[inline]
fn e_bin(v: u32) -> usize {
    ((64 - (u64::from(v) + 1).leading_zeros()) as usize).min(E_BINS) - 1
}

// ---------------------------------------------------------------- traversal shared by stats, encode and decode

type Nb = [Option<u32>; 3];

/// Visits every residual in the Stage 3 coding order. `f(plane, index, [W, N, NW])` returns the residual's value (the
/// encoder reads it from the source, the decoder decodes it); neighbors come only from values already returned.
fn walk(s3: &Stage3Output, mut f: impl FnMut(usize, usize, Nb) -> u32) -> OrganizedResiduals {
    let (width, height) = (s3.metadata().width, s3.metadata().height);
    match s3.mode().expect("stage 2 record") {
        Stage2Mode::Identity => {
            let n = width * height;
            let mut seen = vec![0u32; n];
            let mut res = Vec::with_capacity(n);
            let mut k = 0usize;
            morton_visit::<()>(width, height, |x, y| {
                let left = (x > 0).then(|| seen[y * width + x - 1]);
                let above = (y > 0).then(|| seen[(y - 1) * width + x]);
                let above_left = (x > 0 && y > 0).then(|| seen[(y - 1) * width + x - 1]);
                let v = f(0, k, [left, above, above_left]);
                seen[y * width + x] = v;
                res.push(v);
                k += 1;
                Ok(())
            })
            .unwrap();
            OrganizedResiduals::Morton(res)
        }
        Stage2Mode::BayerPlanes { planes, .. } => {
            let mut res: [Vec<u32>; 4] = Default::default();
            for (pl, (r, p)) in res.iter_mut().zip(planes).enumerate() {
                let (w, len) = (p.width, p.width * p.height);
                r.reserve_exact(len);
                for i in 0..len {
                    let (x, y) = (i % w, i / w);
                    let left = (x > 0).then(|| r[i - 1]);
                    let above = (y > 0).then(|| r[i - w]);
                    let above_left = (x > 0 && y > 0).then(|| r[i - w - 1]);
                    let v = f(pl, i, [left, above, above_left]);
                    r.push(v);
                }
            }
            OrganizedResiduals::BayerPlanes(res)
        }
    }
}

fn src_at(d: &OrganizedResiduals, pl: usize, i: usize) -> u32 {
    match d {
        OrganizedResiduals::Morton(r) => r[i],
        OrganizedResiduals::BayerPlanes(p) => p[pl][i],
    }
}

// ---------------------------------------------------------------- Stage 4 twin (production binarization + Bit)

#[derive(Clone, Copy)]
struct Model {
    unary: [Bit; E_MAX + 1],
    mant: [Bit; E_MAX + 1],
}

impl Model {
    fn new() -> Self {
        Model { unary: [Bit::NEW; E_MAX + 1], mant: [Bit::NEW; E_MAX + 1] }
    }
    fn code_io<I: BitIo>(&mut self, io: &mut I, v: u32) -> u32 {
        let x = u64::from(v) + 1;
        let e_in = (64 - x.leading_zeros()) as usize;
        let mut e = E_MAX;
        for i in 1..E_MAX {
            let s = &mut self.unary[i];
            let b = io.code(s.p1(), (e_in > i) as u32);
            s.adapt(b);
            if b == 0 {
                e = i;
                break;
            }
        }
        let mut out = 1u64;
        for j in (0..e - 1).rev() {
            let want = ((x >> j) & 1) as u32;
            let b = if j == e - 2 {
                let s = &mut self.mant[e];
                let b = io.code(s.p1(), want);
                s.adapt(b);
                b
            } else {
                io.code(32768, want)
            };
            out = (out << 1) | u64::from(b);
        }
        (out - 1) as u32
    }
}

/// Range-codes the Stage 3 residuals with `k` independent models selected by `ctx`. Returns (payload, context ids).
pub fn pcq_encode(s3: &Stage3Output, k: usize, ctx: impl Fn(Option<u32>, Option<u32>, Option<u32>) -> usize) -> (Vec<u8>, Vec<u8>) {
    let src = s3.data();
    let mut models = vec![Model::new(); k];
    let mut enc = BinEnc::new();
    let mut ids = Vec::new();
    walk(s3, |pl, i, nb| {
        let c = ctx(nb[0], nb[1], nb[2]);
        ids.push(c as u8);
        models[c].code_io(&mut enc, src_at(src, pl, i))
    });
    (enc.finish(), ids)
}

/// Decodes `payload` using only the geometry of `s3`; contexts are rebuilt from decoded residuals.
pub fn pcq_decode(
    payload: &[u8],
    s3: &Stage3Output,
    k: usize,
    ctx: impl Fn(Option<u32>, Option<u32>, Option<u32>) -> usize,
) -> (OrganizedResiduals, Vec<u8>, bool) {
    let mut models = vec![Model::new(); k];
    let mut dec = BinDec::new(payload);
    let mut ids = Vec::new();
    let res = walk(s3, |_, _, nb| {
        let c = ctx(nb[0], nb[1], nb[2]);
        ids.push(c as u8);
        models[c].code_io(&mut dec, 0)
    });
    let exact_end = dec.consumed() == payload.len();
    (res, ids, exact_end)
}

// ---------------------------------------------------------------- statistics

/// Target (bit-length) histograms under three context definitions; each is `contexts * E_BINS` row-major.
pub struct Stats {
    /// By pattern key (KEYS rows).
    pub pat: Vec<u32>,
    /// By current production Stage 3 class (4 rows).
    pub base: Vec<u32>,
    /// By half-octave level of the production score, the rounded mean (LEVELS rows).
    pub avg: Vec<u32>,
}

pub fn collect(s3: &Stage3Output) -> Stats {
    let src = s3.data();
    let mut st = Stats { pat: vec![0; KEYS * E_BINS], base: vec![0; 4 * E_BINS], avg: vec![0; LEVELS * E_BINS] };
    walk(s3, |pl, i, nb| {
        let v = src_at(src, pl, i);
        let e = e_bin(v);
        st.pat[key(nb[0], nb[1], nb[2]) * E_BINS + e] += 1;
        st.base[classify_activity(nb[0], nb[1], nb[2]).index() * E_BINS + e] += 1;
        let (mut sum, mut n) = (0u64, 0u64);
        for x in nb.into_iter().flatten() {
            sum += u64::from(x);
            n += 1;
        }
        let score = if n == 0 { 0 } else { ((sum + n / 2) / n) as u32 };
        st.avg[level(score) * E_BINS + e] += 1;
        v
    });
    st
}

// ---------------------------------------------------------------- offline optimization

fn code_len(c: &[f64; E_BINS], n: f64) -> f64 {
    if n <= 0.0 {
        return 0.0;
    }
    let mut s = n * n.log2();
    for &x in c {
        if x > 0.0 {
            s -= x * x.log2();
        }
    }
    s
}

/// Greedy agglomerative partition of the observed pattern keys (rows of `counts`) minimizing the increase in total
/// ideal code length of the target histogram at every merge (weighted Jensen-Shannon cost, the agglomerative
/// information-bottleneck rule). Returns, for each requested context count K (descending as reached), a full
/// KEYS-entry table. Context ids are ordered by ascending mean target bit length. Keys never observed in training are
/// assigned the context of the nearest observed key by L1 distance on the three levels (ties: lowest key).
pub fn agglomerate(counts: &[u32], checkpoints: &[usize]) -> Vec<(usize, Vec<u8>)> {
    let seen: Vec<usize> = (0..KEYS).filter(|&k| counts[k * E_BINS..(k + 1) * E_BINS].iter().any(|&c| c > 0)).collect();
    let n = seen.len();
    let mut c: Vec<[f64; E_BINS]> = seen
        .iter()
        .map(|&k| {
            let mut a = [0f64; E_BINS];
            for (d, &s) in a.iter_mut().zip(&counts[k * E_BINS..]) {
                *d = f64::from(s);
            }
            a
        })
        .collect();
    let mut tot: Vec<f64> = c.iter().map(|a| a.iter().sum()).collect();
    let mut len: Vec<f64> = c.iter().zip(&tot).map(|(a, &t)| code_len(a, t)).collect();
    let pair = |c: &[[f64; E_BINS]], tot: &[f64], len: &[f64], i: usize, j: usize| -> f32 {
        let mut m = [0f64; E_BINS];
        for e in 0..E_BINS {
            m[e] = c[i][e] + c[j][e];
        }
        (code_len(&m, tot[i] + tot[j]) - len[i] - len[j]) as f32
    };
    let mut mat = vec![0f32; n * n];
    for i in 0..n {
        for j in i + 1..n {
            let d = pair(&c, &tot, &len, i, j);
            mat[i * n + j] = d;
            mat[j * n + i] = d;
        }
    }
    let mut active = vec![true; n];
    let mut na = n;
    let mut nn = vec![usize::MAX; n];
    let mut nnd = vec![f32::MAX; n];
    let rescan = |mat: &[f32], active: &[bool], i: usize| -> (usize, f32) {
        let (mut b, mut bd) = (usize::MAX, f32::MAX);
        for j in 0..n {
            if j != i && active[j] && mat[i * n + j] < bd {
                bd = mat[i * n + j];
                b = j;
            }
        }
        (b, bd)
    };
    for i in 0..n {
        (nn[i], nnd[i]) = rescan(&mat, &active, i);
    }
    let mut parent: Vec<usize> = (0..n).collect();
    fn find(p: &mut [usize], mut x: usize) -> usize {
        while p[x] != x {
            p[x] = p[p[x]];
            x = p[x];
        }
        x
    }
    let mut want: Vec<usize> = checkpoints.to_vec();
    want.sort_unstable_by(|a, b| b.cmp(a));
    let stop = *want.last().unwrap_or(&1);
    let mut out = Vec::new();
    let snapshot = |na: usize, c: &[[f64; E_BINS]], parent: &mut Vec<usize>, out: &mut Vec<(usize, Vec<u8>)>| {
        let mut roots: Vec<usize> = (0..n).filter(|&i| find(parent, i) == i).collect();
        let mean_e = |r: usize| -> f64 { (0..E_BINS).map(|e| c[r][e] * (e as f64 + 1.0)).sum::<f64>() / c[r].iter().sum::<f64>().max(1.0) };
        roots.sort_by(|&a, &b| mean_e(a).partial_cmp(&mean_e(b)).unwrap().then(a.cmp(&b)));
        let mut id_of = vec![0u8; n];
        for (id, &r) in roots.iter().enumerate() {
            id_of[r] = id as u8;
        }
        let mut table = vec![0u8; KEYS];
        let mut have = vec![false; KEYS];
        for (i, &k) in seen.iter().enumerate() {
            table[k] = id_of[find(parent, i)];
            have[k] = true;
        }
        let lv = |k: usize| [k / (LEVELS * LEVELS), (k / LEVELS) % LEVELS, k % LEVELS];
        for k in 0..KEYS {
            if !have[k] {
                let a = lv(k);
                let best = seen.iter().min_by_key(|&&s| (lv(s).iter().zip(&a).map(|(x, y)| x.abs_diff(*y)).sum::<usize>(), s)).unwrap();
                table[k] = table[*best];
            }
        }
        out.push((na, table));
    };
    while na > stop {
        if want.contains(&na) {
            snapshot(na, &c, &mut parent, &mut out);
        }
        let i = (0..n).filter(|&i| active[i]).min_by(|&a, &b| nnd[a].partial_cmp(&nnd[b]).unwrap().then(a.cmp(&b))).unwrap();
        let j = nn[i];
        for e in 0..E_BINS {
            c[i][e] += c[j][e];
        }
        tot[i] += tot[j];
        len[i] = code_len(&c[i], tot[i]);
        active[j] = false;
        parent[j] = i;
        na -= 1;
        for k in 0..n {
            if active[k] && k != i {
                let d = pair(&c, &tot, &len, i, k);
                mat[i * n + k] = d;
                mat[k * n + i] = d;
            }
        }
        for k in 0..n {
            if !active[k] {
                continue;
            }
            if k == i || nn[k] == i || nn[k] == j {
                (nn[k], nnd[k]) = rescan(&mat, &active, k);
            } else if mat[k * n + i] < nnd[k] {
                nn[k] = i;
                nnd[k] = mat[k * n + i];
            }
        }
    }
    snapshot(na, &c, &mut parent, &mut out);
    out
}

/// Bits per sample of the target histogram `val` under per-context distributions estimated from `train`
/// (additive smoothing 0.5 per bin). Both are `contexts * E_BINS`.
pub fn xent(train: &[u32], val: &[u32]) -> f64 {
    let a = 0.5;
    let (mut bits, mut n) = (0.0, 0.0);
    for (t, v) in train.chunks(E_BINS).zip(val.chunks(E_BINS)) {
        let tn: f64 = t.iter().map(|&x| f64::from(x)).sum();
        for e in 0..E_BINS {
            let cnt = f64::from(v[e]);
            if cnt > 0.0 {
                bits -= cnt * ((f64::from(t[e]) + a) / (tn + a * E_BINS as f64)).log2();
                n += cnt;
            }
        }
    }
    bits / n.max(1.0)
}

/// Aggregates pattern-key histograms into `nctx` rows chosen by `f(key) -> row`.
pub fn aggregate(pat: &[u32], nctx: usize, f: impl Fn(usize) -> usize) -> Vec<u32> {
    let mut out = vec![0u32; nctx * E_BINS];
    for k in 0..KEYS {
        let r = f(k);
        for e in 0..E_BINS {
            out[r * E_BINS + e] += pat[k * E_BINS + e];
        }
    }
    out
}

// ---------------------------------------------------------------- frozen universal mapping

/// Context count of the frozen mapping.
pub const FROZEN_K: usize = crate::lisa_pcq_table::PCQ_K;

/// Context id of the frozen mapping. Total over all inputs: every neighbor combination maps to a key in `0..KEYS`.
#[inline]
pub fn frozen_ctx(left: Option<u32>, above: Option<u32>, above_left: Option<u32>) -> usize {
    crate::lisa_pcq_table::PCQ_TABLE[key(left, above, above_left)] as usize
}

// ---------------------------------------------------------------- scalar control: trained quantizer of the average

/// Resolution of the scalar feature: the production score (rounded mean of the available neighbors) is clamped to
/// `0..SBINS`.
pub const SBINS: usize = 1024;

/// The production Stage 3 score (see `classify_activity`), clamped to `0..SBINS`.
#[inline]
pub fn score_bin(left: Option<u32>, above: Option<u32>, above_left: Option<u32>) -> usize {
    let (mut sum, mut n) = (0u64, 0u64);
    for v in [left, above, above_left].into_iter().flatten() {
        sum += u64::from(v);
        n += 1;
    }
    if n == 0 { 0 } else { (((sum + n / 2) / n) as usize).min(SBINS - 1) }
}

/// Target histograms by `score_bin` (`SBINS * E_BINS`, row-major).
pub fn collect_score(s3: &Stage3Output) -> Vec<u32> {
    let src = s3.data();
    let mut h = vec![0u32; SBINS * E_BINS];
    walk(s3, |pl, i, nb| {
        let v = src_at(src, pl, i);
        h[score_bin(nb[0], nb[1], nb[2]) * E_BINS + e_bin(v)] += 1;
        v
    });
    h
}

/// Optimal ordered partition of the score bins into `k` contiguous intervals minimizing total ideal code length of the
/// target histogram (exact dynamic programming). Returns the `SBINS`-entry bin -> context table; ids ascend with score.
pub fn partition_dp(h: &[u32], k: usize) -> Vec<u8> {
    let n = SBINS;
    let mut pre = vec![[0f64; E_BINS]; n + 1];
    for i in 0..n {
        for e in 0..E_BINS {
            pre[i + 1][e] = pre[i][e] + f64::from(h[i * E_BINS + e]);
        }
    }
    let seg = |a: usize, b: usize| -> f64 {
        let mut c = [0f64; E_BINS];
        for e in 0..E_BINS {
            c[e] = pre[b][e] - pre[a][e];
        }
        code_len(&c, c.iter().sum())
    };
    let cost: Vec<Vec<f64>> = (0..n).map(|a| (0..=n).map(|b| if b > a { seg(a, b) } else { 0.0 }).collect()).collect();
    let mut best = vec![vec![f64::MAX; n + 1]; k + 1];
    let mut cut = vec![vec![0usize; n + 1]; k + 1];
    best[0][0] = 0.0;
    for j in 1..=k {
        for b in j..=n {
            for a in j - 1..b {
                let v = best[j - 1][a] + cost[a][b];
                if v < best[j][b] {
                    best[j][b] = v;
                    cut[j][b] = a;
                }
            }
        }
    }
    let mut table = vec![0u8; n];
    let mut b = n;
    for j in (1..=k).rev() {
        let a = cut[j][b];
        for t in &mut table[a..b] {
            *t = (j - 1) as u8;
        }
        b = a;
    }
    table
}
