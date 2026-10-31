"""ENTMAP: Shannon-entropy map of the bench25 crops. Offline statistics only: no codec is run, nothing is encoded.
Usage: python -I tools/entmap_analyze.py <bench25 results dir> <crop dir written by entmap_export> <out dir>

Every number is computed on the exact 1024x1024 crop buffers the bench25 codecs received (SHA-256 re-checked here).
PKON2 features (neighbours, MED, GAP, activity, err-energy, prev-plane buckets, sign classes, P) are re-derived from
src/ellie.rs and src/pkonrel.rs; MED is cross-checked against the harness descriptors in the bench25 manifest.

Estimators (all in bits; per-image totals are reported as bits per pixel = all planes of one pixel):
  plug-in   -sum n(c,s) log2(n(c,s)/n(c))           in-sample conditional entropy, parameters free (biased low: overfits)
  mm        plug-in + sum_c (m_c - 1) / (2 ln 2)     Miller-Madow first-order bias correction (m_c = distinct symbols seen in c)
  adaptive  Dirichlet(alpha=1/2) / KT sequential code length from the same counts (closed form, order independent);
            includes the cost of learning every context's distribution from the image itself (biased high)
Null controls: L2 x a hash of (plane, x, y) with the cardinality of each probed feature (4, 49, 64) - what a context split
worth nothing does to each estimator.
Residual symbols are split exactly as PKON2 binarises them: zigzag z <-> (class c, mantissa, sign) is a bijection, so every
model's class + mantissa + sign code length is a valid code length for z. L0 conditions the sign on the full magnitude,
which makes L0 equal the zero-order entropy H0(z) exactly (asserted); richer models use PKON2's sign classes.
"""
import csv, hashlib, json, math, struct, sys, time
import numpy as np
from scipy.fft import dctn
from scipy.special import gammaln

BENCH, CROPS, OUT = sys.argv[1:4]
ONLY = set(sys.argv[4].split(",")) if len(sys.argv) > 4 else None  # smoke runs only
LN2 = math.log(2)
BL = np.array([int(v).bit_length() for v in range(256)], dtype=np.int64)  # class = bit length of magnitude

# ---------------------------------------------------------------- estimators


def h0(sym, K=256):
    """Zero-order (marginal) empirical entropy, bits/symbol."""
    c = np.bincount(sym.ravel().astype(np.int64), minlength=K).astype(np.float64)
    c = c[c > 0]
    n = c.sum()
    return float(-(c * np.log2(c / n)).sum() / n)


def cond_bits(ctx, sym, K, alpha=0.5):
    """(plug-in, Miller-Madow, adaptive KT) bits for coding sym in [0,K) given integer ctx, summed over all samples."""
    if sym.size == 0:
        return 0.0, 0.0, 0.0
    key = ctx.ravel().astype(np.int64) * K + sym.ravel().astype(np.int64)
    if key.max() < (1 << 24):
        cnt = np.bincount(key)
        u = np.nonzero(cnt)[0]
        n = cnt[u].astype(np.float64)
    else:
        u, n = np.unique(key, return_counts=True)
        n = n.astype(np.float64)
    c = u // K
    cu, inv = np.unique(c, return_inverse=True)
    N = np.bincount(inv, weights=n)
    plug = float(-(n * np.log2(n / N[inv])).sum())
    mm = plug + float((np.bincount(inv) - 1).sum()) / (2 * LN2)
    kt = float((gammaln(N + K * alpha) - gammaln(K * alpha)).sum() - (gammaln(n + alpha) - gammaln(alpha)).sum()) / LN2
    return plug, mm, kt


def model_bits(z, cctx, sctx, mctx):
    """Bits (plug, mm, kt) of coding zigzag residuals z as class|cctx (9 symbols), mantissa|class,mctx (2^(c-1)), sign|sctx."""
    z = z.ravel().astype(np.int64)
    m = (z + 1) >> 1
    c = BL[m]
    s = z & 1
    tot = np.array(cond_bits(cctx.ravel(), c, 9))
    mctx = mctx.ravel()
    for cl in range(2, 8):  # class 8 is m == 128 only: mantissa is deterministic
        sel = c == cl
        if sel.any():
            tot += cond_bits(mctx[sel], m[sel] - (1 << (cl - 1)), 1 << (cl - 1))
    nz = m > 0
    tot += cond_bits(sctx.ravel()[nz], s[nz], 2)
    return tuple(float(v) for v in tot)


def hash_ctx(k, shape, card):
    """Deterministic context carrying no image information: integer hash of (plane, x, y) mod card."""
    yy, xx = np.mgrid[0: shape[0], 0: shape[1]].astype(np.uint64)
    M = np.uint64(0xFFFFFFFF)
    h = ((xx * np.uint64(0x9E3779B1)) ^ (yy * np.uint64(0x85EBCA77)) ^ np.uint64((k + 1) * 0xC2B2AE3D)) & M
    h ^= h >> np.uint64(15)
    h = (h * np.uint64(0x2C1B3C6D)) & M
    h ^= h >> np.uint64(12)
    h = (h * np.uint64(0x297A2D39)) & M
    h ^= h >> np.uint64(15)
    return (h % np.uint64(card)).astype(np.int64)


# ---------------------------------------------------------------- PKON2 features (vectorised copies of the Rust definitions)


def nbrs(p):
    """W, N, NW, NE exactly as ellie::neighbors (border substitutions included)."""
    w = np.empty_like(p)
    w[:, 1:] = p[:, :-1]
    w[1:, 0] = p[:-1, 0]
    w[0, 0] = 0
    n = np.empty_like(p)
    n[1:] = p[:-1]
    n[0] = w[0]
    nw = n.copy()
    nw[1:, 1:] = p[:-1, :-1]
    ne = n.copy()
    ne[1:, :-1] = p[:-1, 1:]
    return w, n, nw, ne


def med(w, n, nw):
    mx, mn = np.maximum(w, n), np.minimum(w, n)
    return np.where(nw >= mx, mn, np.where(nw <= mn, mx, w + n - nw))


def gap(p, w, n, nw, ne):
    ww = w.copy()
    ww[:, 2:] = p[:, :-2]
    nn = n.copy()
    nn[2:] = p[:-2]
    nne = ne.copy()
    nne[2:, :-1] = p[:-2, 1:]
    dh = np.abs(w - ww) + np.abs(n - nw) + np.abs(n - ne)
    dv = np.abs(w - nw) + np.abs(n - nn) + np.abs(ne - nne)
    d = dv - dh
    p4 = 2 * (w + n) + (ne - nw)
    p4 = np.where(d > 32, (p4 + 4 * w) >> 1, np.where(d > 8, (3 * p4 + 4 * w) >> 2,
                  np.where(d < -32, (p4 + 4 * n) >> 1, np.where(d < -8, (3 * p4 + 4 * n) >> 2, p4))))
    g = np.clip((p4 + 2) >> 2, 0, 255)
    return np.where(d > 80, w, np.where(d < -80, n, g))


def wdiff(a, b):
    return np.abs(((a - b) & 255).astype(np.uint8).view(np.int8).astype(np.int32))


act_bucket = lambda a: np.digitize(a, [1, 3, 6, 11, 21, 41, 81])
qd = lambda d: np.digitize(d, [-8, -2, 0, 1, 3, 9])
prev_q = lambda z: np.digitize(z, [1, 3, 9])


def sgn(z):
    return np.where(z == 0, 0, np.where((z & 1) == 1, 1, 2))


def zigzag(r8):
    s = r8.astype(np.int8).astype(np.int32)
    return ((s << 1) ^ (s >> 7)) & 255


def resid(p, pred):
    return ((p - pred) & 255).astype(np.uint8).view(np.int8)


def shift(a, dx, dy):
    """a[y+dy, x+dx] with 0 outside (dy <= 0)."""
    out = np.zeros_like(a)
    H, W = a.shape
    ys, yd = slice(max(0, -dy), H), slice(max(0, -dy) + dy, H + dy)
    if dx >= 0:
        out[ys, : W - dx] = a[yd, dx:]
    else:
        out[ys, -dx:] = a[yd, : W + dx]
    return out


class PlaneCtx:
    """Per-plane causal contexts computed from pixel values p (int32) and that plane's zigzag residuals z."""

    def __init__(self, p, z, zprev):
        w, n, nw, ne = nbrs(p)
        self.act = act_bucket(wdiff(w, nw) + wdiff(n, nw) + wdiff(ne, n))
        mx, mn = np.maximum(w, n), np.minimum(w, n)
        self.br = np.where(nw >= mx, 0, np.where(nw <= mn, 1, 2))
        e = shift(z, -1, 0) + shift(z, 0, -1) + shift(z, -1, -1) + shift(z, 1, -1)
        self.eb = act_bucket(e)
        self.pq = prev_q(zprev)
        self.psg = sgn(zprev)
        self.s3 = sgn(shift(z, -1, 0)) * 9 + sgn(shift(z, 0, -1)) * 3 + sgn(shift(z, 1, -1))
        cls = BL[(z + 1) >> 1]
        self.cls4 = np.clip(cls - 1, 0, 3)

    def spatial(self):
        return (self.act * 8 + self.eb) * 3 + self.br, self.s3 * 4 + self.cls4

    def cross(self):
        c, s = self.spatial()
        return c * 4 + self.pq, s * 3 + self.psg


# ---------------------------------------------------------------- LS predictor (statistical predictability probe; not a codec)

TAPS = [(-1, 0), (0, -1), (-1, -1), (1, -1), (-2, 0), (0, -2), (-2, -1), (2, -1), (-1, -2), (1, -2), (-2, -2), (2, -2)]
XTAPS = [(0, 0), (-1, 0), (0, -1), (-1, -1), (1, -1)]
TILE = 128
COEF_BITS = 16


def ls_design(planes, k):
    H, W = planes[0].shape
    ys, xs = slice(2, H), slice(2, W - 2)
    cols = [planes[k][2 + dy: H + dy, 2 + dx: W - 2 + dx] for dx, dy in TAPS]
    for j in range(k):
        cols += [planes[j][2 + dy: H + dy, 2 + dx: W - 2 + dx] for dx, dy in XTAPS]
    X = np.stack([c.astype(np.float32) for c in cols] + [np.ones((H - 2, W - 4), np.float32)], axis=-1)
    return X, planes[k][ys, xs].astype(np.float64), (ys, xs)


def ls_fit(X, y):
    Xd = X.reshape(-1, X.shape[-1]).astype(np.float64)
    beta, *_ = np.linalg.lstsq(Xd, y.ravel(), rcond=None)
    return beta


def ls_residuals(planes, medz, mode):
    """Zigzag residual planes of a least-squares predictor (interior; MED residual on the 2-pixel border) and coefficient count."""
    out, ncoef = [], 0
    for k, p in enumerate(planes):
        if k >= 3:  # alpha (constant in this corpus): keep MED
            out.append(medz[k])
            continue
        X, y, (ys, xs) = ls_design(planes, k)
        pred = np.empty(y.shape)
        if mode == "global":
            beta = ls_fit(X, y)
            pred = X.astype(np.float64) @ beta
            ncoef += beta.size
        else:
            Hi, Wi = y.shape
            for ty in range(0, Hi, TILE):
                for tx in range(0, Wi, TILE):
                    sl = (slice(ty, ty + TILE), slice(tx, tx + TILE))
                    beta = ls_fit(X[sl], y[sl])
                    pred[sl] = X[sl].astype(np.float64) @ beta
                    ncoef += beta.size
        z = medz[k].copy()
        pr = np.clip(np.rint(pred), 0, 255).astype(np.int32)
        z[ys, xs] = zigzag(resid(p[ys, xs], pr))
        out.append(z)
    return out, ncoef


# ---------------------------------------------------------------- per-plane model ladder


def ladder(planes, zs, extra=None):
    """Per plane: dict model -> (plug bits, kt bits). Models: L0 none, L1 spatial ctx, L2 + previous-plane ctx, and extras."""
    res = []
    for k, (p, z) in enumerate(zip(planes, zs)):
        zprev = zs[k - 1] if k > 0 else np.zeros_like(z)
        cx = PlaneCtx(p, z, zprev)
        zero = np.zeros_like(z)
        r = {"L0": model_bits(z, zero, (z + 1) >> 1, zero)}
        c1, s1 = cx.spatial()
        r["L1"] = model_bits(z, c1, s1, cx.act)
        c2, s2 = cx.cross()
        r["L2"] = model_bits(z, c2, s2, cx.act)
        if extra:
            for name, f in extra.items():
                ce, se = f(k, p, cx, c2, s2)
                r[name] = model_bits(z, ce, se, cx.act)
        res.append(r)
    return res


# ---------------------------------------------------------------- JPEG provenance (source-file headers + lattice evidence on the crop)

ZZ = [0, 1, 8, 16, 9, 2, 3, 10, 17, 24, 32, 25, 18, 11, 4, 5, 12, 19, 26, 33, 40, 48, 41, 34, 27, 20, 13, 6, 7, 14, 21, 28,
      35, 42, 49, 56, 57, 50, 43, 36, 29, 22, 15, 23, 30, 37, 44, 51, 58, 59, 52, 45, 38, 31, 39, 46, 53, 60, 61, 54, 47, 55, 62, 63]
STD_LUMA = np.array([16, 11, 10, 16, 24, 40, 51, 61, 12, 12, 14, 19, 26, 58, 60, 55, 14, 13, 16, 24, 40, 57, 69, 56, 14, 17, 22, 29, 51, 87, 80, 62,
                     18, 22, 37, 56, 68, 109, 103, 77, 24, 35, 55, 64, 81, 104, 113, 92, 49, 64, 78, 87, 103, 121, 120, 101, 72, 92, 95, 98, 112, 100, 103, 99], float).reshape(8, 8)


def parse_jpeg(path):
    """Walk every marker segment and entropy-coded scan up to the first EOI. payload = bytes up to EOI minus APPn/COM segments
    (metadata, EXIF thumbnails); trailing = bytes after EOI (e.g. MPF secondary images), excluded from the payload."""
    b = open(path, "rb").read()
    assert b[:2] == b"\xff\xd8", path
    i, meta, qt, comps, prog, eoi = 2, 0, {}, [], False, None
    while i + 2 <= len(b):
        if b[i] != 0xFF:
            raise ValueError(f"marker expected at {i} in {path}")
        mk = b[i + 1]
        if mk == 0xFF:
            i += 1
            continue
        if mk == 0xD9:
            eoi = i + 2
            break
        if mk in (0xD8, 0x01) or 0xD0 <= mk <= 0xD7:
            i += 2
            continue
        L = struct.unpack(">H", b[i + 2: i + 4])[0]
        seg = b[i + 4: i + 2 + L]
        i += 2 + L
        if mk == 0xDA:  # skip entropy-coded data: FF00 stuffing and RSTn stay inside the scan
            while True:
                j = b.find(b"\xff", i)
                if j < 0 or j + 1 >= len(b):
                    raise ValueError(f"scan runs off the end of {path}")
                nx = b[j + 1]
                if nx == 0xFF:
                    i = j + 1
                elif nx == 0x00 or 0xD0 <= nx <= 0xD7:
                    i = j + 2
                else:
                    i = j
                    break
            continue
        if 0xE0 <= mk <= 0xEF or mk == 0xFE:
            meta += L + 2
        elif mk == 0xDB:
            j = 0
            while j < len(seg):
                pq, tq = seg[j] >> 4, seg[j] & 15
                n = 128 if pq else 64
                vals = struct.unpack(">64H", seg[j + 1: j + 1 + n]) if pq else list(seg[j + 1: j + 1 + n])
                t = np.zeros(64)
                t[ZZ] = np.array(vals, float)
                qt[tq] = t.reshape(8, 8)
                j += 1 + n
        elif mk in (0xC0, 0xC1, 0xC2):
            prog = mk == 0xC2
            nf = seg[5]
            comps = [(seg[6 + 3 * c], seg[7 + 3 * c] >> 4, seg[7 + 3 * c] & 15, seg[8 + 3 * c]) for c in range(nf)]
    assert eoi is not None, f"no EOI in {path}"
    (_, h0_, v0_, tq0) = comps[0]
    sub = "4:4:4" if len(comps) == 1 or all((h, v) == (h0_, v0_) for _, h, v, _ in comps) else \
        {(2, 2): "4:2:0", (2, 1): "4:2:2", (1, 2): "4:4:0"}.get((h0_, v0_), f"{h0_}x{v0_}")
    q = qt[tq0]
    best = min(range(1, 101), key=lambda qq: np.abs(np.clip(np.floor((STD_LUMA * (5000 / qq if qq < 50 else 200 - 2 * qq) + 50) / 100), 1, 255) - q).sum())
    s = 5000 / best if best < 50 else 200 - 2 * best
    fit_err = float(np.abs(np.clip(np.floor((STD_LUMA * s + 50) / 100), 1, 255) - q).mean())
    return {"file_bytes": len(b), "meta_bytes": meta, "trailing_bytes": len(b) - eoi, "payload_bytes": eoi - meta, "progressive": prog, "subsampling": sub,
            "luma_q_mean": float(q.mean()), "luma_q_ac_ge4_frac": float((q >= 4).sum() - (q[0, 0] >= 4)) / 63, "ijg_quality_est": best,
            "ijg_fit_mean_abs_err": fit_err}, q


def lattice(rgb, q, x0, y0):
    """JPEG quantisation-lattice evidence in the decoded crop. Luma Y is re-derived from RGB (JFIF weights), cut into 8x8 blocks on
    the source file's block grid, DCT'd (orthonormal = JPEG FDCT scaling) and divided by the file's luma table. Statistic: mean
    cos(2*pi*F/Q) over AC coefficients with Q >= 2 and |F/Q| >= 0.5 (those that quantise to a non-zero level). Returns
    (aligned grid, grid offset by 4 px as the null control, share of coefficients that qualified on the aligned grid)."""
    Y = rgb[..., 0] * 0.299 + rgb[..., 1] * 0.587 + rgb[..., 2] * 0.114 - 128.0
    mask = q >= 2
    mask[0, 0] = False
    out, share = [], float("nan")
    for off in (0, 4):
        bx, by = (-x0 + off) % 8, (-y0 + off) % 8
        H, W = Y.shape
        nb_y, nb_x = (H - by) // 8, (W - bx) // 8
        blk = Y[by: by + 8 * nb_y, bx: bx + 8 * nb_x].reshape(nb_y, 8, nb_x, 8).transpose(0, 2, 1, 3)
        F = dctn(blk, axes=(2, 3), norm="ortho")
        t = (F[:, :, mask] / q[mask]).ravel()
        t = t[np.abs(t) >= 0.5]
        out.append(float(np.cos(2 * np.pi * t).mean()) if t.size else float("nan"))
        if off == 0:
            share = t.size / (nb_y * nb_x * 63)
    return out[0], out[1], share


def complexity_ratio(path, x0, y0, w, h):
    """Rough crop-vs-whole-image complexity: H0 of the G-plane MED residual on the crop region / on the full image. Uses PIL's
    JPEG decode (not bit-identical to the benchmark decoder; only a ratio is taken)."""
    from PIL import Image
    a = np.asarray(Image.open(path).convert("RGB"))[..., 1].astype(np.int32)

    def hmed(p):
        w_, n, nw, _ = nbrs(p)
        return h0(zigzag(resid(p, med(w_, n, nw))))
    return hmed(a[y0: y0 + h, x0: x0 + w]) / hmed(a)


# ---------------------------------------------------------------- non-local repetition descriptor


def repeat_stats(rgb, medz_any):
    """Raster-order last occurrence of the exact RGB (W,N,NW,NE) context: share of interior pixels whose context was seen
    before, share predicted exactly by that last occurrence, and the same share restricted to pixels MED did not predict exactly."""
    a = rgb.astype(np.uint64)
    H, W = a.shape[:2]
    pix = (a[..., 0] << 16) | (a[..., 1] << 8) | a[..., 2]
    ys, xs = slice(1, H), slice(1, W - 1)
    k1 = (pix[1:, :-2] << 24) | pix[:-1, 1:-1]   # W, N
    k2 = (pix[:-1, :-2] << 24) | pix[:-1, 2:]    # NW, NE
    v = pix[ys, xs].ravel()
    k1, k2 = k1.ravel(), k2.ravel()
    pos = np.arange(v.size)
    o = np.lexsort((pos, k2, k1))
    same = np.zeros(v.size, bool)
    same[1:] = (k1[o][1:] == k1[o][:-1]) & (k2[o][1:] == k2[o][:-1])
    hit = np.zeros(v.size, bool)
    hit[1:] = same[1:] & (v[o][1:] == v[o][:-1])
    seen = np.zeros(v.size, bool)
    seen[o] = same
    hits = np.zeros(v.size, bool)
    hits[o] = hit
    miss = medz_any[ys, xs].ravel()
    return float(seen.mean()), float(hits.mean()), float(hits[miss].mean()) if miss.any() else 0.0, float(miss.mean())


# ---------------------------------------------------------------- main

man = json.load(open(f"{BENCH}/manifest_pre_encode.json", encoding="utf-8"))
index = {e["name"]: e for e in json.load(open(f"{CROPS}/index.json", encoding="utf-8"))}
long_rows, wide_rows, jpeg_rows, checks = [], [], [], []
t_all = time.time()
for m in man:
    t0 = time.time()
    name = m["name"]
    if ONLY and name not in ONLY:
        continue
    e = index[name]
    w_, h_, ch = e["w"], e["h"], e["channels"]
    raw = np.fromfile(f"{CROPS}/{name}.raw", dtype=np.uint8)
    sha = hashlib.sha256(struct.pack("<II", w_, h_) + bytes([ch, int(ch in (2, 4)), 8]) + raw.tobytes()).hexdigest()
    assert sha == m["crop_sha256"], f"crop sha mismatch {name}"
    img = raw.reshape(h_, w_, ch)
    npx = w_ * h_
    alpha_const = bool(ch == 4 and (img[..., 3] == img[0, 0, 3]).all())
    chan = [img[..., c].astype(np.int32) for c in range(ch)]
    rgb_planes = [chan[1], chan[0], chan[2]] + chan[3:]           # PKON2 coding order G,R,B,(A)
    sub = [rgb_planes[0], (rgb_planes[1] - rgb_planes[0] + 128) & 255, (rgb_planes[2] - rgb_planes[0] + 128) & 255] + rgb_planes[3:]

    # harness cross-check: MED on G, interior, no wrap
    g = chan[1]
    a_, b_, c_ = g[1:, :-1], g[:-1, 1:], g[:-1, :-1]
    pr = np.where(c_ >= np.maximum(a_, b_), np.minimum(a_, b_), np.where(c_ <= np.minimum(a_, b_), np.maximum(a_, b_), a_ + b_ - c_))
    rr = np.abs(g[1:, 1:] - pr)
    chk = {"image": name, "crop_sha256_ok": True, "med_mae_g": float(rr.mean()), "harness_med_mae_g": m["med_mae_g"],
           "med_zero_g": float((rr == 0).mean()), "harness_med_zero_g": m["med_zero_frac_g"], "alpha_constant": alpha_const if ch == 4 else None}
    assert abs(chk["med_mae_g"] - m["med_mae_g"]) < 1e-9 and abs(chk["med_zero_g"] - m["med_zero_frac_g"]) < 1e-12, chk

    W = {"image": name, "channels": ch, "raw_bpp": 8 * ch}

    # ---- A. zero-order marginals
    W["H0_channels"] = sum(h0(c) for c in chan)
    W["H0_subg_planes"] = sum(h0(p) for p in sub)
    rgb = img[..., :3].astype(np.int64)
    key = (rgb[..., 0] << 16) | (rgb[..., 1] << 8) | rgb[..., 2]
    _, cnt = np.unique(key, return_counts=True)
    cnt = cnt.astype(float)
    W["H_joint_rgb_plugin"] = float(-(cnt * np.log2(cnt / npx)).sum() / npx)
    W["unique_colors"] = int(cnt.size)
    hr, hg, hb = (h0(chan[c]) for c in range(3))
    W["TC_rgb_plugin"] = hr + hg + hb - W["H_joint_rgb_plugin"]
    for (u, v_, lab) in ((0, 1, "I_RG"), (2, 1, "I_BG"), (0, 2, "I_RB")):
        joint = chan[u] * 256 + chan[v_]
        W[lab] = h0(chan[u]) + h0(chan[v_]) - h0(joint, 65536)
    # ---- B. spatial conditioning directly on pixel values (G plane)
    gw, gn, gnw, gne = nbrs(chan[1])
    W["G_H0"] = hg
    W["G_H_given_W_plugin"], W["G_H_given_W_mm"], W["G_H_given_W_kt"] = (x / npx for x in cond_bits(gw, chan[1], 256))
    W["G_H_given_WN_plugin"], W["G_H_given_WN_mm"], W["G_H_given_WN_kt"] = (x / npx for x in cond_bits(gw * 256 + gn, chan[1], 256))

    # ---- C. MED residuals: RGB domain vs SubG domain (PKON2 representation)
    def med_z(planes):
        out = []
        for p in planes:
            w, n, nw, ne = nbrs(p)
            out.append(zigzag(resid(p, med(w, n, nw))))
        return out
    z_rgb, z_sub = med_z(rgb_planes), med_z(sub)
    W["H0_med_rgb"] = sum(h0(z) for z in z_rgb)
    W["H0_med_subg"] = sum(h0(z) for z in z_sub)
    rG = resid(rgb_planes[0], med(*nbrs(rgb_planes[0])[:3])).astype(np.int32) & 255
    rR = resid(rgb_planes[1], med(*nbrs(rgb_planes[1])[:3])).astype(np.int32) & 255
    rB = resid(rgb_planes[2], med(*nbrs(rgb_planes[2])[:3])).astype(np.int32) & 255
    W["I_resid_RG"] = h0(rR) + h0(rG) - h0(rR * 256 + rG, 65536)
    W["I_resid_BG"] = h0(rB) + h0(rG) - h0(rB * 256 + rG, 65536)

    # ---- D. context-model ladders
    jp = None
    x0, y0 = (e["full_w"] - w_) // 2, (e["full_h"] - h_) // 2
    yy, xx = np.mgrid[0:h_, 0:w_]
    par = (yy & 1) * 2 + (xx & 1)
    ph = ((yy + y0) % 8) * 8 + ((xx + x0) % 8)

    def p_feat(k, p, cx, c2, s2):
        w, n, nw, ne = nbrs(p)
        md = med(w, n, nw)
        pf = qd(gap(p, w, n, nw, ne) - md) * 7 + qd(np.clip(w + n - nw, 0, 255) - md)
        return c2 * 49 + pf, s2 * 49 + pf

    def dummy(card):
        def f(k, p, cx, c2, s2):
            hc = hash_ctx(k, p.shape, card)
            return c2 * card + hc, s2 * card + hc
        return f

    extra = {"L3_P": p_feat,
             "L2_parity": lambda k, p, cx, c2, s2: (c2 * 4 + par, s2 * 4 + par),
             "L2_phase8": lambda k, p, cx, c2, s2: (c2 * 64 + ph, s2 * 64 + ph),
             "L2_null4": dummy(4), "L2_null49": dummy(49), "L2_null64": dummy(64)}
    lad_sub = ladder(sub, z_sub, extra)
    lad_rgb = ladder(rgb_planes, z_rgb)
    zls_g, nc_g = ls_residuals(sub, z_sub, "global")
    zls_t, nc_t = ls_residuals(sub, z_sub, "tile")
    lad_lsg = ladder(sub, zls_g)
    lad_lst = ladder(sub, zls_t)
    W["H0_ls_global"] = sum(h0(z) for z in zls_g)
    W["H0_ls_tile"] = sum(h0(z) for z in zls_t)
    W["ls_global_coef_bits"] = nc_g * COEF_BITS
    W["ls_tile_coef_bits"] = nc_t * COEF_BITS

    for dom, lad, extra_bits in (("subg_med", lad_sub, 0), ("rgb_med", lad_rgb, 0), ("subg_ls_global", lad_lsg, nc_g * COEF_BITS),
                                 ("subg_ls_tile", lad_lst, nc_t * COEF_BITS)):
        for model in lad[0]:
            plug, mm, kt = (sum(r[model][i] for r in lad) for i in range(3))
            W[f"{dom}:{model}:plugin"] = plug / npx
            W[f"{dom}:{model}:mm"] = mm / npx
            W[f"{dom}:{model}:adaptive"] = (kt + extra_bits) / npx
            for k, r in enumerate(lad):
                long_rows.append({"image": name, "domain": dom, "model": model, "plane": k, "plugin_bits_per_sample": r[model][0] / npx,
                                  "mm_bits_per_sample": r[model][1] / npx, "adaptive_bits_per_sample": r[model][2] / npx})
    # chain-rule self-check: L0 plug-in equals the zero-order entropy of z; plug-in is the minimum of the three estimators
    assert abs(sum(r["L0"][0] for r in lad_sub) / npx - W["H0_med_subg"]) < 1e-6
    assert all(r[mo][0] <= min(r[mo][1], r[mo][2]) + 1e-6 for lad in (lad_sub, lad_rgb, lad_lsg, lad_lst) for r in lad for mo in r)

    # ---- E. non-local repetition
    miss_any = np.zeros((h_, w_), bool)
    for z in z_sub:
        miss_any |= z != 0
    W["ctx_seen_share"], W["repeat_hit_share"], W["repeat_hit_share_of_med_miss"], W["med_miss_share"] = repeat_stats(img[..., :3], miss_any)

    # ---- F. JPEG provenance
    if m["source_format"] == "image/jpeg":
        info, q = parse_jpeg(m["source_path"])
        la, lm, ls_ = lattice(img[..., :3].astype(np.float64), q, x0, y0)
        cr = complexity_ratio(m["source_path"], x0, y0, w_, h_)
        full_bpp = info["payload_bytes"] * 8 / (e["full_w"] * e["full_h"])
        info.update({"image": name, "full_w": e["full_w"], "full_h": e["full_h"], "crop_x0": x0, "crop_y0": y0,
                     "crop_share_of_full": npx / (e["full_w"] * e["full_h"]), "src_payload_bpp_fullimage": full_bpp,
                     "crop_complexity_ratio": cr, "src_payload_bpp_crop_adjusted": full_bpp * cr,
                     "lattice_aligned": la, "lattice_offset4": lm, "lattice_coef_share": ls_})
        jpeg_rows.append(info)
        W["src_jpeg_payload_bpp"] = full_bpp
        W["src_jpeg_payload_bpp_crop_adj"] = full_bpp * cr
        W["lattice_aligned"] = la
        W["lattice_offset4"] = lm
    checks.append(chk)
    wide_rows.append(W)
    f3 = lambda k: "/".join(f"{W[f'subg_med:{k}:{e}']:.2f}" for e in ("plugin", "mm", "adaptive"))
    print(f"{name:14s} {time.time() - t0:5.1f}s  H0ch {W['H0_channels']:.2f}  H0med {W['H0_med_subg']:.2f}  L2 {f3('L2')}  L3 {f3('L3_P')}"
          f"  null49 {f3('L2_null49')}  ph8 {f3('L2_phase8')}  null64 {f3('L2_null64')}", flush=True)


def wcsv(path, rows):
    keys = list(dict.fromkeys(k for r in rows for k in r))
    with open(path, "w", newline="", encoding="utf-8") as f:
        wr = csv.DictWriter(f, fieldnames=keys)
        wr.writeheader()
        wr.writerows(rows)


wcsv(f"{OUT}/entropy_per_image.csv", wide_rows)
wcsv(f"{OUT}/entropy_per_plane.csv", long_rows)
wcsv(f"{OUT}/jpeg_provenance.csv", jpeg_rows)
json.dump({"checks": checks, "params": {"kt_alpha": 0.5, "ls_taps": TAPS, "ls_cross_taps": XTAPS, "ls_tile": TILE, "coef_bits": COEF_BITS},
           "elapsed_s": time.time() - t_all}, open(f"{OUT}/analysis_checks.json", "w", encoding="utf-8"), indent=1)
print(f"done {time.time() - t_all:.0f}s")
