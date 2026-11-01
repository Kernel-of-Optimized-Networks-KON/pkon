"""Analytical derivation only: reads frozen artifacts, runs no codec, no benchmark. Writes results.json + pareto_frontier.png."""
import json, csv, math
import numpy as np
import matplotlib
matplotlib.use("Agg")
import matplotlib.pyplot as plt

B = "../../"
NEW = B + "ellie-full-bench/20261101-000449Z-ellie-vs-bench50/"
OLD = B + "pkon2/20261027-155433Z-bench50/"
S3 = B + "ellie-stage4-3img/20261031-223101Z-stage4-three-image/results.json"
nj = json.load(open(NEW + "results.json", encoding="utf-8"))
img = {i["name"]: i for i in nj["images"]}
rec = {}
for r in nj["records"]:
    rec.setdefault(r["image"], {})[r["codec"]] = r
old = {}
for r in csv.DictReader(open(OLD + "per_image.csv", encoding="utf-8")):
    old.setdefault(r["image"], {})[r["codec"]] = r

names = list(img)
RAW = sum(rec[n]["ellie"]["raw_bytes"] for n in names)  # = samples (8-bit)
PX = sum(rec[n]["ellie"]["pixels"] for n in names)
E_B = sum(rec[n]["ellie"]["total_bytes"] for n in names)
IDEAL = sum(rec[n]["ellie"]["ellie_side"]["ideal_stage3_bytes"] for n in names)
MODEL_MS = sum(rec[n]["ellie"]["ellie_side"]["model_only_enc_ms"] for n in names)
ENC_MS = sum(rec[n]["ellie"]["enc_ms"] for n in names)
NET_MS = 16547.0  # report.md "enc net" (process floor subtracted)
assert RAW == 158334976 and E_B == 69186801 and PX == 52428800
cmp_ = {c: sum(rec[n][c]["total_bytes"] for n in names) for c in ["jxl-e7-t1", "jxl-e9-t1", "webp-m6-t1", "png-rs"]}
legacy_file = sum(int(old[n]["pkon2-file"]["total_bytes"]) for n in names)
legacy_pay = sum(int(old[n]["pkon2-file"]["payload_bytes"]) for n in names)
# machine-drift normalisation: the same competitor codecs were timed in both sessions (report timing tables, MP/s net)
old_t = {"jxl-e7-t1": 1.09, "jxl-e9-t1": 0.25, "webp-m6-t1": 0.10, "png-rs": 1.75}
new_t = {"jxl-e7-t1": 1.13, "jxl-e9-t1": 0.28, "webp-m6-t1": 0.12, "png-rs": 1.97}
drift = {c: new_t[c] / old_t[c] for c in old_t}
d_lo, d_hi = drift["jxl-e7-t1"], drift["png-rs"]
leg_lo, leg_hi = 3.08 * d_lo, 3.08 * d_hi

ns_total = NET_MS * 1e6 / RAW
ns_model = MODEL_MS * 1e6 / RAW
ns_coder = (ENC_MS - MODEL_MS) * 1e6 / RAW
s3 = json.load(open(S3, encoding="utf-8"))
dec = []
for i in s3["images"]:
    r = i["runs"][0]
    n = i["canonical_bytes"]
    dec.append(dict(image=i["image"], dec_per_sample=r["binary_decisions"] / n, bits_per_sample=r["bits_per_sample"],
                    model_ns_per_dec=r["model_only_encode_s_best"] / r["binary_decisions"] * 1e9,
                    coder_ns_per_dec=(r["encode_s_best"] - r["model_only_encode_s_best"]) / r["binary_decisions"] * 1e9))
mnd = [d["model_ns_per_dec"] for d in dec]
cnd = [d["coder_ns_per_dec"] for d in dec]
dps = [d["dec_per_sample"] for d in dec]

p_min = 32 / 65536
floor_bits = -math.log2(1 - p_min)
bits_per_sample = E_B * 8 / RAW
clamp_rel_max = floor_bits * max(dps) / bits_per_sample


def mant_loss(theta):
    a = np.arange(0, 4096)
    p = np.where(a == 0, 1.0, 2 * theta ** a)
    p = p / p.sum()
    loss = 0.0
    for k in range(4, 13):
        w = 2 ** (k - 3)
        for lo in range(2 ** (k - 1), 2 ** k, w):
            pp = p[lo:lo + w]
            s = pp.sum()
            if s > 0:
                q = pp / s
                nz = q > 0
                loss += s * (math.log2(w) + (q[nz] * np.log2(q[nz])).sum())
    H = -(p[p > 0] * np.log2(p[p > 0])).sum() + (1 - p[0])
    return loss, H


ml = []
for theta in np.linspace(0.3, 0.985, 60):
    l, H = mant_loss(theta)
    ml.append((theta, l, H, l / H))
mant_abs_max = max(x[1] for x in ml)
mant_rel_max = max(x[3] for x in ml)


def hgeo(mae):
    lo, hi = 0.0, 0.999999
    for _ in range(100):
        q = (lo + hi) / 2
        if 2 * q / ((1 - q) * (1 + q)) < mae:
            lo = q
        else:
            hi = q
    q = lo
    z = (1 + q) / (1 - q)
    x = np.arange(-3000, 3001)
    p = q ** np.abs(x) / z
    p = p[p > 0]
    return float(-(p * np.log2(p)).sum())


rows = []
for n in names:
    px = rec[n]["ellie"]["pixels"]
    rows.append(dict(image=n, mae=img[n]["med_mae_g"], h0_ceiling_bps=hgeo(img[n]["med_mae_g"]),
                     ellie_bps=rec[n]["ellie"]["total_bytes"] * 8 / px / 3,
                     jxl9_bps=rec[n]["jxl-e9-t1"]["total_bytes"] * 8 / px / 3,
                     legacy_bps=int(old[n]["pkon2-file"]["payload_bytes"]) * 8 / px / 3,
                     ellie_bytes=rec[n]["ellie"]["total_bytes"], legacy_payload=int(old[n]["pkon2-file"]["payload_bytes"]),
                     jxl9_bytes=rec[n]["jxl-e9-t1"]["total_bytes"], pixels=px))
sum_h0 = sum(r["h0_ceiling_bps"] * r["pixels"] * 3 / 8 for r in rows)
below = sum(r["ellie_bps"] < r["h0_ceiling_bps"] for r in rows)
srt = sorted(rows, key=lambda r: r["mae"])
terc = {}
for nm, g in [("low MAE (smooth)", srt[:17]), ("mid MAE", srt[17:34]), ("high MAE (noisy)", srt[34:])]:
    e = sum(r["ellie_bytes"] for r in g)
    l = sum(r["legacy_payload"] for r in g)
    terc[nm] = dict(n=len(g), mae_range=[g[0]["mae"], g[-1]["mae"]], ellie_bytes=e, legacy_payload=l, gap_bytes=e - l,
                    ellie_over_legacy=e / l)
tg = sum(v["gap_bytes"] for v in terc.values())
for v in terc.values():
    v["share_of_total_gap"] = v["gap_bytes"] / tg

gap_bytes = E_B - legacy_pay
mixer_lo = min(mnd) / 5 * min(dps) / ns_total * 100
mixer_hi = max(mnd) / 5 * max(dps) / ns_total * 100
res = dict(
    note="Analytical only. No codec/benchmark was run.",
    sources=dict(new=NEW, legacy=OLD, stage4_3img=S3, code="src/ellie.rs"),
    baseline=dict(raw_bytes=RAW, pixels=PX, encoded_bytes=E_B, ratio=RAW / E_B, bits_per_sample=bits_per_sample,
                  enc_MPs=3.17, dec_MPs=2.59, stage3_ideal_bytes=IDEAL, stage4_overhead_bytes=E_B - IDEAL,
                  stage4_overhead_pct=(E_B / IDEAL - 1) * 100),
    speed_budget=dict(min_enc_MPs=3.17, ns_per_sample_total=ns_total, ns_per_sample_stage1to3=ns_model,
                      ns_per_sample_stage4=ns_coder, stage4_share_pct=ns_coder / (ns_model + ns_coder) * 100,
                      note="Stage1-3 (null-coder run) and full encode are separate best-of-2 measurements"),
    per_decision_3img=dict(per_image=dec, model_ns_per_decision_range=[min(mnd), max(mnd)],
                           coder_ns_per_decision_range=[min(cnd), max(cnd)], decisions_per_sample_range=[min(dps), max(dps)],
                           caveat="3 images, different session; bench50 does not store decision counts -> UNKNOWN for bench50"),
    references_same_corpus=dict(
        jxl_e7=dict(bytes=cmp_["jxl-e7-t1"], ratio=RAW / cmp_["jxl-e7-t1"], MPs=1.13),
        jxl_e9=dict(bytes=cmp_["jxl-e9-t1"], ratio=RAW / cmp_["jxl-e9-t1"], MPs=0.28),
        webp_m6=dict(bytes=cmp_["webp-m6-t1"], ratio=RAW / cmp_["webp-m6-t1"], MPs=0.12),
        png_rs=dict(bytes=cmp_["png-rs"], ratio=RAW / cmp_["png-rs"], MPs=1.97),
        legacy_pkon2_X4PJ=dict(file_bytes=legacy_file, payload_bytes=legacy_pay, ratio_payload=RAW / legacy_pay,
                               ratio_file=RAW / legacy_file, MPs_measured_older_session=3.08, drift_factors=drift,
                               MPs_drift_normalised_range=[leg_lo, leg_hi],
                               caveat="retired codec, source not in tree, timed in an earlier session; existence evidence only")),
    bounds_zero_compute=dict(
        stage4_overhead_bytes=E_B - IDEAL,
        prob_clamp_floor=dict(p_min=p_min, bits_per_certain_decision=floor_bits, max_relative_saving_pct_loose_cap=clamp_rel_max * 100,
                              basis="all decisions certain at max observed 6.97 decisions/sample; unattainable cap"),
        raw_mantissa=dict(max_loss_bits_per_sample=mant_abs_max, max_relative_pct=mant_rel_max * 100,
                          basis="two-sided geometric residual, theta 0.3..0.985; KL(true||uniform) inside 2^(k-3) subbuckets, k>=4")),
    added_compute_estimates=dict(mixer_input_pct_of_budget=[mixer_lo, mixer_hi],
                                 basis="model ns/decision / 5 mixer terms x decisions/sample, over 104.5 ns/sample; even-split assumption"),
    diagnostic_order0=dict(
        basis="max-entropy: integer X with E|X|=m has H <= two-sided geometric(m). G-plane MED MAE from bench50 metadata, ASSUMED equal for R,B",
        ceiling_total_bytes=sum_h0, ellie_bytes=E_B, ellie_below_ceiling_pct=(1 - E_B / sum_h0) * 100,
        images_ellie_below_ceiling=below, jxl9_below_ceiling_pct=(1 - cmp_["jxl-e9-t1"] / sum_h0) * 100,
        legacy_below_ceiling_pct=(1 - legacy_pay / sum_h0) * 100),
    gap_to_reference=dict(bytes=gap_bytes, pct_of_ellie=gap_bytes / E_B * 100, ratio_from=RAW / E_B, ratio_to=RAW / legacy_pay,
                          ratio_improvement_x=(RAW / legacy_pay) / (RAW / E_B), by_mae_tercile=terc,
                          gap_vs_jxl_e9_bytes=E_B - cmp_["jxl-e9-t1"]),
    per_image=rows,
)
json.dump(res, open("results.json", "w"), indent=1)

C = dict(ink="#1f2430", mut="#5b6270", grid="#d9dce2", cur="#c2410c", leg="#1d4ed8", cmp="#6b7280", bnd="#0f766e")
plt.rcParams.update({"font.size": 10})
fig, (ax, ax2) = plt.subplots(1, 2, figsize=(16, 7), gridspec_kw=dict(width_ratios=[1.3, 1]))
ax.set_xscale("log")
ax.set_xlim(0.08, 12)
ax.set_ylim(1.8, 4.6)
ax.grid(color=C["grid"], lw=.6)
ax.set_axisbelow(True)
ax.axvspan(0.08, 3.17, color="#000", alpha=0.04, lw=0)
ax.axvline(3.17, color=C["cur"], lw=1.6, ls="--")
ax.text(3.3, 1.86, "min speed 3.17 MP/s\n(left of line: too slow)", color=C["cur"], fontsize=9, va="bottom")
r_cur, r_leg = RAW / E_B, RAW / legacy_pay
ax.fill_between([3.17, 12], r_cur, r_leg, color=C["leg"], alpha=0.10, lw=0)
ax.fill_between([3.17, 12], r_cur, r_cur * (1 + 0.0014), color=C["bnd"], alpha=0.9, lw=0)
ax.text(3.4, 3.5, "Reference-only gain region:\nbounded above by a measured retired codec\non this corpus; NOT proven recoverable", color=C["leg"], fontsize=9)
ax.text(3.4, 2.36, "Proven at zero compute: <= ~0.14% (teal sliver)", color=C["bnd"], fontsize=9)
ax.plot([0.08, 3.08], [r_leg, r_leg], color=C["ink"], lw=1.2)
ax.plot([3.08, 3.08], [r_leg, r_cur], color=C["ink"], lw=1.2)
ax.plot([3.08, 3.17], [r_cur, r_cur], color=C["ink"], lw=1.2)
ax.text(0.4, r_leg + 0.04, "measured non-dominated frontier (staircase)", fontsize=8.5)
for n_, x, y in [("JXL e7", 1.13, RAW / cmp_["jxl-e7-t1"]), ("JXL e9", .28, RAW / cmp_["jxl-e9-t1"]),
                 ("WebP m6", .12, RAW / cmp_["webp-m6-t1"]), ("PNG (rs)", 1.97, RAW / cmp_["png-rs"])]:
    ax.plot(x, y, "s", color=C["cmp"], ms=8, mec="white", mew=1.2)
    ax.annotate(n_, (x, y), xytext=(6, -15), textcoords="offset points", color=C["mut"], fontsize=9)
ax.plot([leg_lo, leg_hi], [r_leg, r_leg], color=C["leg"], lw=4, alpha=.4, solid_capstyle="butt")
ax.plot(3.08, r_leg, "D", color=C["leg"], ms=9, mec="white", mew=1.2)
ax.annotate(f"Retired ELLIE-generation codec: {r_leg:.3f}x\n3.08 MP/s measured (older session);\nbar = {leg_lo:.2f}-{leg_hi:.2f} if normalised for\nmachine drift (assumption)",
            (3.08, r_leg), xytext=(-330, 30), textcoords="offset points", color=C["leg"], fontsize=8.8)
ax.plot(3.17, r_cur, "o", color=C["cur"], ms=11, mec="white", mew=1.4, zorder=5)
ax.annotate(f"ELLIE now\n{r_cur:.3f}x @ 3.17 MP/s", (3.17, r_cur), xytext=(-120, -40), textcoords="offset points",
            color=C["cur"], fontsize=10, fontweight="bold")
ax.annotate("", xy=(6.5, r_leg), xytext=(6.5, r_cur), arrowprops=dict(arrowstyle="<->", color=C["leg"], lw=1.4))
ax.text(6.9, (r_cur + r_leg) / 2, f"gap {r_leg - r_cur:.2f}x\n{gap_bytes / 1e6:.1f} MB\n= {gap_bytes / E_B * 100:.1f}% of\nELLIE size", color=C["leg"], fontsize=9, va="center")
ax.set_xlabel("Encode throughput, MP/s (log scale, single thread, net)")
ax.set_ylabel("Aggregate compression ratio (raw / stored), bench50")
ax.set_xticks([.1, .3, 1, 3, 10])
ax.set_xticklabels(["0.1", "0.3", "1", "3", "10"])
ax.set_title("Compression vs encode speed, 50-image bench50 corpus\nsquares: competitors | circle: ELLIE now | diamond: retired codec (older session)", fontsize=10.5, loc="left")

rowsL = [
    ("Stage 4 coder overhead", 0, 0, f"saves <= {E_B - IDEAL:.0f} B total (proven)", C["bnd"]),
    ("Probability clamp / 12-bit floor", None, None, f"gain cap {clamp_rel_max * 100:.2f}% (loose); cost UNKNOWN", C["bnd"]),
    ("Model the raw mantissa bits", None, None, f"gain <= {mant_rel_max * 100:.2f}% (geometric model);\ncost >= 1 modelled decision per k>=4 sample", C["bnd"]),
    ("Add 1 mixer input to every decision", mixer_lo, mixer_hi, "estimate (even-split assumption); gain UNKNOWN", C["cmp"]),
    ("Richer contexts / cross-plane / Stage 1-2", None, None, "cost UNKNOWN, gain UNKNOWN\n(reference gap exists: 43.8%)", C["leg"]),
]
ax2.set_xlim(-2, 45)
ax2.set_ylim(len(rowsL) - .4, -.6)
for i, (lab, lo, hi, txt, col) in enumerate(rowsL):
    ax2.text(-1.8, i - .36, lab, fontsize=9.5, va="center", fontweight="bold")
    if lo is not None and hi is not None and hi > lo:
        ax2.barh(i, hi - lo, left=lo, height=.26, color=col, alpha=.8)
    elif lo is not None:
        ax2.plot(lo, i, "o", color=col, ms=8)
    elif i == 0:
        ax2.plot(0, i, "o", color=col, ms=8)
    else:
        ax2.text(1, i, "?  not estimated", fontsize=9, color=col, va="center", style="italic")
    ax2.text(-1.8, i + .3, txt, fontsize=8.8, color=C["mut"], va="center")
ax2.axvline(0, color=C["mut"], lw=.8)
ax2.set_yticks([])
ax2.set_xlabel("Added encode time, % of 104.5 ns/sample budget (>0 breaks 3.17 MP/s unless offset)")
ax2.set_title("Opportunities: estimated added compute vs bounded gain\n'?' = unknown, deliberately not estimated", fontsize=10.5, loc="left")
for s in ["top", "right", "left"]:
    ax2.spines[s].set_visible(False)
fig.text(0.01, 0.005, "Measured: bench50 runs. Everything else is bounded/estimated and labelled. No new runs. Per-decision cost from 3-image Stage 4 experiment (17-22 ns/decision).", fontsize=7.8, color=C["mut"])
fig.tight_layout(rect=(0, .02, 1, 1))
fig.savefig("pareto_frontier.png", dpi=150)

print(json.dumps({k: res[k] for k in ["baseline", "speed_budget", "bounds_zero_compute", "added_compute_estimates", "diagnostic_order0"]}, indent=1))
print("gap", gap_bytes, gap_bytes / E_B * 100, r_cur, r_leg, "drift", drift, leg_lo, leg_hi)
print(json.dumps(terc, indent=1))
print(min(mnd), max(mnd), min(cnd), max(cnd), min(dps), max(dps))
