"""Derive report tables from metrics_long.csv (additive sums -> ratios). Usage: python -I summarize.py <result_dir>"""
import csv, math, sys, collections

d = sys.argv[1]
M = collections.defaultdict(dict)
for r in csv.DictReader(open(f"{d}/metrics_long.csv")):
    M[(r["image"], r["plane"])][r["key"]] = float(r["value"])
CL = ["zero", "unary", "mant_mod", "mant_raw", "sign"]
PN = ["Y/R", "Co/G", "Cg/B"]
PART = ["order0(node)", "qs", "B(qs,qd)", "C(qe,qw)", "B+qw", "B+qe", "B+qe+qw", "sign(sW,sN)"]
out = []
P = out.append


def stat(m, t):
    n = m[f"{t}.n"]
    mean = m[f"{t}.sum"] / n
    return dict(n=n, mn=m[f"{t}.min"], mx=m[f"{t}.max"], mean=mean, mabs=m[f"{t}.sumabs"] / n,
                var=m[f"{t}.sumsq"] / n - mean * mean, z=100 * m[f"{t}.zeros"] / n,
                H=m[f"{t}.H0_signed_bits"] / n, Hm=m[f"{t}.H0_mag_bits"] / n,
                Hs=m[f"{t}.H_sign_bits"] / n, nz=1 - m[f"{t}.zeros"] / n)


keys = sorted(M.keys(), key=lambda k: (k[0] == "AGGREGATE", k[0], k[1] == "ALL", k[1]))
P("## A. Residual statistics (e = Stage 1 signed residual, r = Stage 2 corrected residual)\n")
P("| image | plane | px | tag | min | max | mean | mean abs | var | zero % | H0(signed) b/px | H0(mag) b/px | nz*H(sign) b/px | H0(mag)+nz*H(sign) |")
P("|---|---|---|---|---|---|---|---|---|---|---|---|---|---|")
for k in keys:
    for t in "er":
        s = stat(M[k], t)
        P(f"| {k[0]} | {k[1]} | {int(s['n'])} | {t} | {s['mn']:.0f} | {s['mx']:.0f} | {s['mean']:+.4f} | {s['mabs']:.3f} | {s['var']:.2f} | {s['z']:.2f} | {s['H']:.4f} | {s['Hm']:.4f} | {s['nz']*s['Hs']:.4f} | {s['Hm']+s['nz']*s['Hs']:.4f} |")

P("\n## B. Magnitude distribution by bit length k (share of samples, r)\n")
P("| image | plane | k=0 (zero) | k=1 (|r|=1) | k=2 (2-3) | k=3 (4-7) | k=4 (8-15) | k=5 (16-31) | k>=6 |")
P("|---|---|---|---|---|---|---|---|---|")
for k in keys:
    m = M[k]
    n = m["r.n"]
    kl = [m[f"r.klog{i:02}"] / n * 100 for i in range(20)]
    P(f"| {k[0]} | {k[1]} | " + " | ".join(f"{kl[i]:.2f}" for i in range(6)) + f" | {sum(kl[6:]):.2f} |")

P("\n## C. Stage-by-stage bit accounting (bits per pixel; bits total in last cols)\n")
P("| image | plane | H0(e) | H0(r) | model cost (sum -log2 p1) | interval (coder) | payload (Stage 4 bytes*8) | arith loss | termination+align | payload/H0(r) |")
P("|---|---|---|---|---|---|---|---|---|---|")
for k in keys:
    m = M[k]
    n = m["px"]
    he, hr = m["e.H0_signed_bits"], m["r.H0_signed_bits"]
    mo, iv, pl = m["model_bits"], m["interval_bits"], m["payload_bits"]
    P(f"| {k[0]} | {k[1]} | {he/n:.4f} | {hr/n:.4f} | {mo/n:.4f} | {iv/n:.4f} | {pl/n:.4f} | {iv-mo:.1f} b | {pl-iv:.1f} b | {pl/hr:.4f} |")

P("\n## D. Container overhead (per image)\n")
P("| image | file bytes | header | meta | payload bytes | container % of file |")
P("|---|---|---|---|---|---|")
for k in keys:
    if k[1] != "ALL":
        continue
    m = M[k]
    pb = m["payload_bits"] / 8
    if k[0] == "AGGREGATE":
        fb = sum(M[(i, "ALL")]["file_bytes"] for i in ("portrait_0", "screen_0", "night_0"))
        hb = sum(M[(i, "ALL")]["header_bytes"] for i in ("portrait_0", "screen_0", "night_0"))
        mb = sum(M[(i, "ALL")]["meta_bytes"] for i in ("portrait_0", "screen_0", "night_0"))
    else:
        fb, hb, mb = m["file_bytes"], m["header_bytes"], m["meta_bytes"]
    P(f"| {k[0]} | {fb:.0f} | {hb:.0f} | {mb:.0f} | {pb:.0f} | {(hb+mb)/fb*100:.4f} |")

P("\n## E. Decision-category breakdown (production probabilities, Bc255S11)\n")
P("| image | plane | class | decisions | share of decisions | model bits | share of cost | bits/decision | ones frac | order-0 node entropy bits | model - order0 | model vs order0 % |")
P("|---|---|---|---|---|---|---|---|---|---|---|---|")
for k in keys:
    m = M[k]
    tm = m["model_bits"]
    tn = sum(m[f"cls{c}.n"] for c in range(5))
    for c in range(5):
        n, mo = m[f"cls{c}.n"], m[f"cls{c}.model"]
        if n == 0:
            continue
        o0 = m[f"part0.static.{c}"]
        P(f"| {k[0]} | {k[1]} | {CL[c]} | {n:.0f} | {n/tn*100:.1f}% | {mo:.0f} | {mo/tm*100:.2f}% | {mo/n:.4f} | {m[f'cls{c}.ones']/n:.4f} | {o0:.0f} | {mo-o0:+.0f} | {(mo-o0)/o0*100 if o0 else 0:+.2f}% |")

P("\n## E2. Stage 2 counterfactual (production Stage 3 on e vs on r; cost only, not a production configuration)\n")
P("| image | plane | model bits on r (production) | model bits on e (Stage 2 bypassed) | delta bits (r - e) | delta % | H0(r)-H0(e) b/px |")
P("|---|---|---|---|---|---|---|")
for k in keys:
    m = M[k]
    r_, e_ = m["model_bits"], m["cf_no_stage2.model_bits"]
    P(f"| {k[0]} | {k[1]} | {r_:.0f} | {e_:.0f} | {r_-e_:+.0f} | {(r_-e_)/e_*100:+.3f}% | {(m['r.H0_signed_bits']-m['e.H0_signed_bits'])/m['px']:+.4f} |")

P("\n## F. Model cost vs empirical benchmarks per decision class (bits; lower than model = structure the model did not use)\n")
P("Static = empirical conditional entropy of the observed decisions given the partition (no learning cost; optimistic for sparse partitions).")
P("KT = adaptive Krichevsky-Trofimov code length for the same partition (learning cost included; an honest, causal comparator).\n")
for k in keys:
    if k[1] != "ALL":
        continue
    m = M[k]
    P(f"### {k[0]}\n")
    P("| class | production model | order0(node) KT | qs KT | B KT | C KT | B+qw KT | B+qe KT | B+qe+qw KT | sign(sW,sN) KT | B static | C static | B+qe+qw static |")
    P("|---|---|---|---|---|---|---|---|---|---|---|---|---|")
    for c in range(5):
        r = [m[f"cls{c}.model"]] + [m[f"part{p}.kt.{c}"] for p in range(8)] + [m[f"part{p}.static.{c}"] for p in (2, 3, 6)]
        P(f"| {CL[c]} | " + " | ".join(f"{x:.0f}" for x in r) + " |")
    tot = [m["model_bits"]] + [sum(m[f"part{p}.kt.{c}"] for c in range(5)) for p in range(8)] + [sum(m[f"part{p}.static.{c}"] for c in range(5)) for p in (2, 3, 6)]
    P("| **all** | " + " | ".join(f"{x:.0f}" for x in tot) + " |")
    P("")

P("## G. Autocorrelation (pooled Pearson, lag = (dx, dy) = pixel at x-dx, y-dy)\n")
P("| image | tag | kind | " + " | ".join(f"({dx},{dy})" for dx, dy in [(1, 0), (2, 0), (3, 0), (0, 1), (0, 2), (1, 1), (-1, 1), (2, 1), (-2, 1)]) + " |")
P("|---|---|---|---|---|---|---|---|---|---|---|---|")
for im in ("portrait_0", "screen_0", "night_0", "AGGREGATE"):
    m = M[(im, "ALL")]
    for t in "er":
        for kind in ("signed", "abs"):
            vals = []
            for dx, dy in [(1, 0), (2, 0), (3, 0), (0, 1), (0, 2), (1, 1), (-1, 1), (2, 1), (-2, 1)]:
                # pooled: each plane stored cov/var (one value per plane overlap-normalised?) -> divide by planes' contribution
                vals.append(m[f"{t}.acf_{kind}.{dx}.{dy}"] / m[f"{t}.acf_cnt.{dx}.{dy}"])
            P(f"| {im} | {t} | {kind} | " + " | ".join(f"{v:.4f}" for v in vals) + " |")

P("\n## H. Production model calibration by qs context (aggregate, all classes pooled; excess = model bits - order0-in-qs KT bits)\n")
P("| qs | decisions | model bits | bits/decision | mean predicted P(1) | observed P(1) | calib. gap |")
P("|---|---|---|---|---|---|---|")
m = M[("AGGREGATE", "ALL")]
for qs in range(32):
    n = sum(m.get(f"qs{qs:02}.n.{c}", 0) for c in range(5))
    if not n:
        continue
    mo = sum(m[f"qs{qs:02}.model.{c}"] for c in range(5))
    ps = sum(m[f"qs{qs:02}.psum.{c}"] for c in range(5)) / n
    ob = sum(m[f"qs{qs:02}.ones.{c}"] for c in range(5)) / n
    P(f"| {qs} | {n:.0f} | {mo:.0f} | {mo/n:.4f} | {ps:.4f} | {ob:.4f} | {ps-ob:+.4f} |")

open(f"{d}/summary.md", "w").write("\n".join(out) + "\n")
print("wrote", f"{d}/summary.md")
