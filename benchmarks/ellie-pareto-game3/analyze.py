"""Aggregate exp_ellie_pareto CSVs: per-config totals, deltas vs PROD, Pareto frontier (bytes vs encode time).
usage: analyze.py <csv> [<csv>...]   (writes markdown to stdout)
Totals: bytes = sum of payload bytes; times = sum over images of the per-image minimum over reps (the established best-of
convention); *_med = sum of per-image medians (noise indicator). Ratio = raw bytes / payload bytes."""
import csv, sys, collections

rows = []
for f in sys.argv[1:]:
    rows += list(csv.DictReader(open(f)))
agg = collections.OrderedDict()
for r in rows:
    a = agg.setdefault(r["cfg"], dict(bytes=0, enc=0.0, dec=0.0, encm=0.0, decm=0.0, ok=True, det=True, ident=True, n=0, per={}))
    a["bytes"] += int(r["bytes"]); a["enc"] += float(r["enc_min_s"]); a["dec"] += float(r["dec_min_s"])
    a["encm"] += float(r["enc_med_s"]); a["decm"] += float(r["dec_med_s"])
    a["ok"] &= r["exact"] == "true"; a["det"] &= r["det"] == "true"; a["ident"] &= r["prod_identical"] == "true"; a["n"] += 1
    a["per"][r["image"]] = (int(r["bytes"]), float(r["enc_min_s"]), float(r["dec_min_s"]))
base = agg["PROD"]
n = base["n"]
px = n * 1024 * 1024  # all crops are 1024x1024 (every source image is larger)
imgs = [r["image"] for r in rows if r["cfg"] == "PROD"]
raw = 158334976 if n == 50 else sum(1024 * 1024 * (4 if i == "screen_0" else 3) for i in imgs)  # screen_0.png is RGBA (alpha plane coded unchanged)
print(f"images: {n}\n")
print("| config | bytes | ratio* | d size | enc s | d enc | enc MP/s | dec s | d dec | enc spread | ok | det |")
print("|---|---|---|---|---|---|---|---|---|---|---|---|")
pts = []
for k, a in agg.items():
    spread = (a["encm"] - a["enc"]) / a["enc"] * 100
    print(f"| {k} | {a['bytes']} | {raw/a['bytes']:.3f} | {(a['bytes']/base['bytes']-1)*100:+.2f}% | {a['enc']:.2f} | {(a['enc']/base['enc']-1)*100:+.1f}% | {px/1e6/a['enc']:.2f} | {a['dec']:.2f} | {(a['dec']/base['dec']-1)*100:+.1f}% | {spread:.0f}% | {a['ok']} | {a['det']} |")
    pts.append((k, a["bytes"], a["enc"], a["dec"]))
dom = lambda p, q: q[1] <= p[1] and q[2] <= p[2] and (q[1] < p[1] or q[2] < p[2])
front = sorted([p for p in pts if not any(dom(p, q) for q in pts)], key=lambda p: p[2])
print("\nPareto frontier (bytes vs encode time, minima):")
for k, b, e, d in front:
    print(f"- {k}: {b} B ({(b/base['bytes']-1)*100:+.2f}%), enc {e:.2f}s ({(e/base['enc']-1)*100:+.1f}%), dec {d:.2f}s ({(d/base['dec']-1)*100:+.1f}%)")
