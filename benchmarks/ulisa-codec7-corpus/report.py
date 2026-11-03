"""results.jsonl (+ manifest.json, split.json) -> per_image.csv, timings.csv, results.json, report_data.md. Run with `python -I report.py <bench_dir>`.
Aggregates are over images where every codec produced an exact reconstruction (the Bayer images); anything else is listed separately."""
import csv, json, os, statistics, sys

d = sys.argv[1]
rows_in = [json.loads(l) for l in open(os.path.join(d, "results.jsonl"), encoding="utf-8")]
floor = next(r["floor"] for r in rows_in if "floor" in r)
imgs = [r for r in rows_in if "id" in r]
man = {m["id"]: m for m in json.load(open(os.path.join(d, "manifest.json")))}
split = json.load(open(os.path.join(d, "split.json")))
sp = {i: s for s in ("train", "val", "test") for i in split[s]}
C = ["lisa4", "ellie6", "ulisa7", "jxl_e7", "jxl_e9", "png"]
NAME = {"lisa4": "LISA (4)", "ellie6": "ELLIE (6)", "ulisa7": "ULISA (7)", "jxl_e7": "JXL e7", "jxl_e9": "JXL e9", "png": "PNG"}
CAT = {  # sensor family, for the "image types" question
    "pentax": "Pentax APS-C/FF DSLR", "leica": "Leica", "sigma": "Sigma fp", "hasselblad": "Hasselblad MF", "ricoh": "Ricoh GR", "nikon": "Nikon", "samsung": "Samsung", "canon": "Canon compact",
    "pixel6pro": "phone", "nexus6": "phone", "lg": "phone", "dji": "drone", "parrot": "drone", "bmpcc4k": "cine", "x100s": "X-Trans"}
for i in imgs:
    i["valid"] = all(i.get(c, {}).get("exact") is True for c in C)
ok = [i for i in imgs if i["valid"]]
bad = [i for i in imgs if not i["valid"]]
px = sum(i["pixels"] for i in ok)
raw = sum(i["raw_bytes"] for i in ok)


def tot(sel, c, k):
    return sum(i[c][k] for i in sel)


def pct(a, b):
    return (a / b - 1) * 100


with open(os.path.join(d, "per_image.csv"), "w", newline="") as f:
    w = csv.writer(f)
    w.writerow(["image", "split", "width", "height", "bit_depth", "raw_bytes", "codec", "exact", "bytes", "bpp", "ratio", "sha256", "file"])
    ext = {"lisa4": "lisa4.pkon", "ellie6": "ellie6.pkon", "ulisa7": "ulisa7.pkon", "jxl_e7": "jxl-e7.jxl", "jxl_e9": "jxl-e9.jxl", "png": "png"}
    for i in imgs:
        for c in C:
            r = i[c]
            b = r.get("bytes")
            w.writerow([i["id"], sp[i["id"]], i["width"], i["height"], i["bit_depth"], i["raw_bytes"], c, r.get("exact", False), b or "",
                        f'{b * 8 / i["pixels"]:.4f}' if b else "", f'{i["raw_bytes"] / b:.4f}' if b else "", r.get("sha256", ""), f'files/{i["id"]}.{ext[c]}' if b else r.get("error", "")])
with open(os.path.join(d, "timings.csv"), "w", newline="") as f:
    w = csv.writer(f)
    w.writerow(["image", "pixels", "codec", "enc_s_best_of_2", "dec_s_best_of_2", "enc_Mpx_s", "dec_Mpx_s"])
    for i in imgs:
        for c in C:
            r = i[c]
            if "enc_s" in r:
                w.writerow([i["id"], i["pixels"], c, f'{r["enc_s"]:.4f}', f'{r["dec_s"]:.4f}', f'{i["pixels"] / r["enc_s"] / 1e6:.3f}', f'{i["pixels"] / r["dec_s"] / 1e6:.3f}'])


def agg(sel):
    px_ = sum(i["pixels"] for i in sel)
    raw_ = sum(i["raw_bytes"] for i in sel)
    o = {"n": len(sel), "pixels": px_, "raw_bytes": raw_, "codecs": {}}
    for c in C:
        b = tot(sel, c, "bytes")
        fe = floor["enc_s"] * len(sel) if c.startswith("jxl") else 0
        fd = floor["dec_s"] * len(sel) if c.startswith("jxl") else 0
        o["codecs"][c] = {"bytes": b, "bpp": b * 8 / px_, "ratio": raw_ / b, "enc_s": tot(sel, c, "enc_s"), "dec_s": tot(sel, c, "dec_s"),
                          "enc_net_s": tot(sel, c, "enc_s") - fe, "dec_net_s": tot(sel, c, "dec_s") - fd,
                          "exact": sum(i[c]["exact"] is True for i in sel)}
    return o


def wl(sel, a, b):
    wins = [i for i in sel if i[a]["bytes"] < i[b]["bytes"]]
    ties = [i for i in sel if i[a]["bytes"] == i[b]["bytes"]]
    losses = [i for i in sel if i[a]["bytes"] > i[b]["bytes"]]
    per = [pct(i[a]["bytes"], i[b]["bytes"]) for i in sel]
    return {"wins": len(wins), "ties": len(ties), "losses": len(losses), "median_pct": statistics.median(per) if per else None,
            "mean_pct": statistics.mean(per) if per else None, "best_pct": min(per) if per else None, "worst_pct": max(per) if per else None,
            "win_ids": [i["id"] for i in wins], "loss_ids": [i["id"] for i in losses]}


groups = {"all_valid": ok, "train": [i for i in ok if sp[i["id"]] == "train"], "val": [i for i in ok if sp[i["id"]] == "val"], "test_heldout": [i for i in ok if sp[i["id"]] == "test"]}
groups["train+val"] = groups["train"] + groups["val"]
by_depth = {}
for i in ok:
    by_depth.setdefault(f'{i["bit_depth"]}-bit', []).append(i)
by_fam = {}
for i in ok:
    by_fam.setdefault(CAT.get(i["id"].split("_")[0], i["id"].split("_")[0]), []).append(i)
res = {"floor_ffmpeg_s": floor, "images": len(imgs), "valid_images": [i["id"] for i in ok], "invalid_or_na_images": [{"id": i["id"], "detail": {c: i[c].get("error") or ("mismatch" if i[c].get("exact") is False else None) for c in C}} for i in bad],
       "aggregate": {g: agg(s) for g, s in groups.items() if s},
       "by_bit_depth": {g: agg(s) for g, s in by_depth.items()}, "by_family": {g: agg(s) for g, s in by_fam.items()},
       "pairs": {f"{a}_vs_{b}": {g: wl(s, a, b) for g, s in groups.items() if s} for a in ("ulisa7", "ellie6") for b in C if a != b},
       "per_image": imgs}
json.dump(res, open(os.path.join(d, "results.json"), "w"), indent=1)

# ---- markdown data tables (the narrative in report.md is written by hand around these)
o = []
P = o.append


def table(h, rows):
    return "\n".join(["| " + " | ".join(h) + " |", "|" + "---|" * len(h)] + ["| " + " | ".join(map(str, r)) + " |" for r in rows])


def agg_table(a, ref="ulisa7"):
    rows = []
    for c in C:
        x = a["codecs"][c]
        base = a["codecs"][ref]["bytes"]
        rows.append([NAME[c], f'{x["bytes"]:,}', f'{x["bpp"]:.4f}', f'{x["ratio"]:.4f}', "-" if c == ref else f'{pct(base, x["bytes"]):+.2f}%', f'{x["exact"]}/{a["n"]}'])
    return table(["codec", "total bytes", "bpp (of 16-bit container pixel)", "ratio", "ULISA size vs codec", "exact"], rows)


for g in ("all_valid", "train+val", "test_heldout"):
    if g in res["aggregate"]:
        a = res["aggregate"][g]
        P(f'### {g}: {a["n"]} images, {a["pixels"]:,} px, {a["raw_bytes"]:,} raw bytes\n')
        P(agg_table(a))
        P("")
P("### Timing, all valid images (sum of best-of-2 seconds; net = minus ffmpeg per-call floor)\n")
a = res["aggregate"]["all_valid"]
rows = []
for c in C:
    x = a["codecs"][c]
    rows.append([NAME[c], f'{x["enc_s"]:.1f}', f'{x["enc_net_s"]:.1f}', f'{a["pixels"] / x["enc_net_s"] / 1e6:.3f}', f'{x["dec_s"]:.1f}', f'{x["dec_net_s"]:.1f}', f'{a["pixels"] / x["dec_net_s"] / 1e6:.3f}'])
P(table(["codec", "enc s", "enc net s", "enc Mpx/s", "dec s", "dec net s", "dec Mpx/s"], rows))
P("")
P("### Pairwise wins/losses by file size (negative % = first codec smaller)\n")
rows = []
for a_, b_ in [("ulisa7", "ellie6"), ("ulisa7", "lisa4"), ("ulisa7", "jxl_e7"), ("ulisa7", "jxl_e9"), ("ulisa7", "png"), ("ellie6", "lisa4"), ("ellie6", "jxl_e9")]:
    for g in ("all_valid", "train+val", "test_heldout"):
        if g in res["aggregate"]:
            x = res["pairs"][f"{a_}_vs_{b_}"][g]
            rows.append([f"{NAME[a_]} vs {NAME[b_]}", g, f'{x["wins"]}/{x["ties"]}/{x["losses"]}', f'{x["median_pct"]:+.2f}%', f'{x["mean_pct"]:+.2f}%', f'{x["best_pct"]:+.2f}%', f'{x["worst_pct"]:+.2f}%'])
P(table(["pair", "set", "wins/ties/losses", "median", "mean", "best", "worst"], rows))
P("")
P("### Per image (bytes; ULISA % vs each; split)\n")
rows = []
for i in imgs:
    if not i["valid"]:
        continue
    u = i["ulisa7"]["bytes"]
    rows.append([i["id"], sp[i["id"]], f'{i["width"]}x{i["height"]}', i["bit_depth"]] + [f'{i[c]["bytes"]:,}' for c in C] + [f'{pct(u, i[c]["bytes"]):+.2f}%' for c in ("ellie6", "lisa4", "jxl_e7", "jxl_e9", "png")])
P(table(["image", "split", "WxH", "bits"] + [NAME[c] for c in C] + ["U vs ELLIE", "U vs LISA", "U vs e7", "U vs e9", "U vs PNG"], rows))
P("")
P("### Per image timing (best-of-2 seconds enc/dec)\n")
rows = [[i["id"]] + [f'{i[c]["enc_s"]:.2f}/{i[c]["dec_s"]:.2f}' for c in C] for i in imgs if i["valid"]]
P(table(["image"] + [NAME[c] for c in C], rows))
P("")
P("### By bit depth / sensor family (ULISA vs ELLIE, vs JXL e9; aggregate bytes)\n")
rows = []
for kind, grp in (("bit depth", by_depth), ("family", by_fam)):
    for g, s in sorted(grp.items()):
        c_ = {c: tot(s, c, "bytes") for c in C}
        x = wl(s, "ulisa7", "ellie6")
        rows.append([kind, g, len(s), f'{pct(c_["ulisa7"], c_["ellie6"]):+.2f}%', f'{x["wins"]}/{len(s)}', f'{pct(c_["ulisa7"], c_["jxl_e9"]):+.2f}%', f'{pct(c_["ulisa7"], c_["png"]):+.2f}%'])
P(table(["grouping", "group", "n", "ULISA vs ELLIE", "ULISA beats ELLIE", "ULISA vs JXL e9", "ULISA vs PNG"], rows))
P("")
if bad:
    P("### Images not in the aggregates\n")
    for i in bad:
        P(f'- {i["id"]}: ' + "; ".join(f"{c}: {i[c].get('error') or ('exact=' + str(i[c].get('exact')))}" for c in C))
    P("")
open(os.path.join(d, "report_data.md"), "w", encoding="utf-8").write("\n".join(o) + "\n")
print("\n".join(o))
