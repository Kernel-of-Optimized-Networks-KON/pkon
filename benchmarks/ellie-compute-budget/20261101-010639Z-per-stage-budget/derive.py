"""Analytical derivation only: reads frozen artifacts, runs no codec/benchmark. Writes results.json and compute_budget.csv."""
import json, csv
import numpy as np

NEW = "../../ellie-full-bench/20261101-000449Z-ellie-vs-bench50/results.json"
S3I = "../../ellie-stage4-3img/20261031-223101Z-stage4-three-image/results.json"
d = json.load(open(NEW, encoding="utf-8"))
recs = [r for r in d["records"] if r["codec"] == "ellie"]
RAW = sum(r["raw_bytes"] for r in recs)
PX = sum(r["pixels"] for r in recs)
ENC_MS = sum(r["enc_ms"] for r in recs)
MODEL_MS = sum(r["ellie_side"]["model_only_enc_ms"] for r in recs)
NET_MS = 16547.0  # report.md: enc net (process floor subtracted)
T = NET_MS * 1e6 / RAW                       # total ns/sample (the budget)
M = MODEL_MS * 1e6 / RAW                     # Stages 1-3, null coder
S4 = (ENC_MS - MODEL_MS) * 1e6 / RAW         # Stage 4 by difference
exact_mps = PX / (NET_MS / 1e3) / 1e6

# --- fit A: 3-image experiment, model-only ns/sample = F + b * decisions/sample
s3 = json.load(open(S3I, encoding="utf-8"))
pts = []
for i in s3["images"]:
    r = i["runs"][0]
    n = i["canonical_bytes"]
    pts.append(dict(image=i["image"], dps=r["binary_decisions"] / n, model_ns=r["model_only_encode_s_best"] / n * 1e9,
                    full_ns=r["encode_s_best"] / n * 1e9, coder_ns_per_sample=(r["encode_s_best"] - r["model_only_encode_s_best"]) / n * 1e9))
X = np.array([[1, p["dps"]] for p in pts])
y = np.array([p["model_ns"] for p in pts])
(Fa, ba), *_ = np.linalg.lstsq(X, y, rcond=None)
resid_a = (X @ np.array([Fa, ba]) - y).tolist()
dps_mean_est = (M - Fa) / ba                 # decisions/sample implied for bench50 (derived, low confidence)
coder_ns_per_dec = [p["coder_ns_per_sample"] / p["dps"] for p in pts]

# --- fit B: 50 images, model-only ns/sample = F + g * bits/sample (content proxy; decisions not stored)
xs = np.array([r["total_bytes"] * 8 / r["raw_bytes"] for r in recs])
ys = np.array([r["ellie_side"]["model_only_enc_ms"] * 1e6 / r["raw_bytes"] for r in recs])
A = np.vstack([np.ones_like(xs), xs]).T
(Fb, gb), *_ = np.linalg.lstsq(A, ys, rcond=None)
res = ys - A @ np.array([Fb, gb])
s2 = (res ** 2).sum() / (len(xs) - 2)
cov = s2 * np.linalg.inv(A.T @ A)
Fb_se = float(np.sqrt(cov[0, 0]))
r2 = 1 - (res ** 2).sum() / ((ys - ys.mean()) ** 2).sum()

# --- session-to-session variation, same images, ELLIE encode ms: 3-image experiment vs bench50
bm = {r["image"]: r["enc_ms"] for r in recs}
var = {}
for i in s3["images"]:
    var[i["image"]] = dict(bench50_ms=bm.get(i["image"]), exp_ms=i["runs"][0]["encode_s_best"] * 1e3)
for k, v in var.items():
    v["exp_vs_bench50_pct"] = (v["exp_ms"] / v["bench50_ms"] - 1) * 100 if v["bench50_ms"] else None

F_lo, F_hi = min(Fa, Fb), max(Fa, Fb)         # upper bound on (S1 + S2 + S3 per-sample part)
S3_lo, S3_hi_from_F = M - F_hi, M - F_lo      # lower bounds on Stage 3 from the two fits

def pct(v):
    return v / T * 100

rows = [
    dict(stage="Stage 1 (prediction)", cost_lo=None, cost_hi=None, kind="not isolated (bounded jointly with Stage 2)"),
    dict(stage="Stage 2 (bias)", cost_lo=None, cost_hi=None, kind="not isolated (bounded jointly with Stage 1)"),
    dict(stage="Stage 1+2 combined", cost_lo=None, cost_hi=F_hi, kind="derived upper bound"),
    dict(stage="Stage 3 (probability model)", cost_lo=S3_lo, cost_hi=M, kind="derived lower bound; upper bound = Stages 1-3 total"),
    dict(stage="Stage 4 (range coder)", cost_lo=S4, cost_hi=S4, kind="measured by difference"),
]
out = dict(
    note="Analytical only. No codec or benchmark was run. Sources: bench50 results.json, stage4-3img results.json, src/ellie.rs.",
    baseline=dict(samples=RAW, pixels=PX, enc_net_ms=NET_MS, enc_MPs_exact=exact_mps, budget_ns_per_sample=T,
                  stage1to3_null_coder_ns_per_sample=M, stage4_by_difference_ns_per_sample=S4, stage4_share_pct=pct(S4),
                  stage1to3_share_pct=pct(M),
                  caveat="model-only and full encode are separate best-of-2 measurements; sum of enc_ms (16590) differs from net (16547) by the per-call floor"),
    fit_3img=dict(points=pts, fixed_ns_per_sample=Fa, ns_per_decision=ba, residuals_ns=resid_a, dof=1,
                  implied_bench50_decisions_per_sample=dps_mean_est, coder_ns_per_decision=coder_ns_per_dec),
    fit_bench50_vs_bits_per_sample=dict(fixed_ns_per_sample=Fb, fixed_se=Fb_se, slope_ns_per_bit_per_sample=gb, r2=r2, n=len(xs)),
    fixed_per_sample_range_ns=[F_lo, F_hi],
    session_variation_same_images=var,
    stage_bounds_ns_per_sample=dict(stage1_plus_2_upper=F_hi, stage3_lower=S3_lo, stage3_upper_if_stage12_were_zero=M, stage4=S4),
    exchange_rates=dict(
        one_pct_of_budget_ns=T / 100,
        five_pct_of_budget_ns=T * 0.05,
        ten_pct_of_budget_ns=T * 0.10,
        ns_per_added_decision_per_sample=ba,
        ten_pct_in_decisions_per_sample=T * 0.10 / ba,
        ten_pct_as_pct_of_mean_decisions=T * 0.10 / ba / dps_mean_est * 100,
    ),
    rows=rows,
)
json.dump(out, open("results.json", "w"), indent=1)

def fmt(lo, hi):
    if lo is None and hi is None:
        return "not isolated"
    if lo is None:
        return f"<= {hi:.1f}"
    if abs(lo - hi) < 1e-9:
        return f"{lo:.1f}"
    return f"{lo:.1f} to {hi:.1f}"

csvrows = [
    ["Stage", "Current cost (ns/sample)", "Current share", "Unallocated headroom (ns/sample)", "Reallocatable headroom (ns/sample)",
     "Max stage cost, others unchanged (ns/sample)", "Measurement basis", "Confidence"],
    ["Stage 1", "not isolated", "not isolated", "0", "unknown (<= Stage 1+2 bound)", "= current", "no stored Stage 1 timing; bounded jointly with Stage 2", "none (unseparated)"],
    ["Stage 2", "not isolated", "not isolated", "0", "unknown (<= Stage 1+2 bound)", "= current", "no stored Stage 2 timing; ~6 integer ops/sample by code inspection", "none (unseparated)"],
    ["Stage 1+2", f"<= {F_hi:.1f}", f"<= {pct(F_hi):.0f}%", "0", "unknown", "= current", "intercept of per-sample cost vs content (two fits: %.1f and %.1f)" % (Fa, Fb), "low-medium (upper bound)"],
    ["Stage 3", f">= {S3_lo:.1f} (and <= {M:.1f})", f">= {pct(S3_lo):.0f}%", "0", "unknown", "= current", "Stages 1-3 null-coder minus Stage 1+2 upper bound", "low-medium (lower bound)"],
    ["Stage 4", f"{S4:.1f}", f"{pct(S4):.1f}%", "0", "unknown (no coder-speed comparison stored)", "= current", "full encode minus null-coder encode, 50 images; 3-image cross-check %.1f-%.1f ns/decision" % (min(coder_ns_per_dec), max(coder_ns_per_dec)), "medium-high (measured by difference)"],
    ["Total", f"{T:.1f}", "100%", "0", "-", f"{T:.1f}", f"net encode {NET_MS:.0f} ms / {RAW} samples = {exact_mps:.4f} MP/s", "high (session variation +-5-10%)"],
]
with open("compute_budget.csv", "w", newline="", encoding="utf-8") as f:
    csv.writer(f).writerows(csvrows)
print(json.dumps({k: out[k] for k in ["baseline", "fit_3img", "fit_bench50_vs_bits_per_sample", "fixed_per_sample_range_ns", "session_variation_same_images", "stage_bounds_ns_per_sample", "exchange_rates"]}, indent=1, default=float))
