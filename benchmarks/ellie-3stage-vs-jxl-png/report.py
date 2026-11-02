"""Aggregate results.json -> per_image.csv + report.md. Aggregate ratios from summed bytes; timing = sum of best-of-2 per image
(`net` subtracts the per-call process floor measured on the 33x17 calibration image)."""
import csv, json, sys, os
d = sys.argv[1]
R = json.load(open(os.path.join(d, "results.json"), encoding="utf-8"))
calib = {c: (e, de) for c, e, de in R["calibration_ms"]}
C = ["ellie", "jxl-e7-t1", "jxl-e9-t1", "png-rs"]
recs = {}
for r in R["records"]:
    recs.setdefault(r["image"], {})[r["codec"]] = r
names = [i["name"] for i in R["images"]]
ok = [n for n in names if all(recs[n][c].get("valid") for c in C)]
tot = lambda c, k: sum(recs[n][c][k] for n in ok)
raw = sum(recs[n]["ellie"]["raw_bytes"] for n in ok); px = sum(recs[n]["ellie"]["pixels"] for n in ok)
def table(h, rows): return "\n".join(["| " + " | ".join(h) + " |", "|" + "---|" * len(h)] + ["| " + " | ".join(map(str, r)) + " |" for r in rows])
o = []; P = o.append
with open(os.path.join(d, "per_image.csv"), "w", newline="") as f:
    w = csv.writer(f); w.writerow(["image", "codec", "valid", "bytes", "raw_bytes", "ratio", "enc_ms", "dec_ms", "sha256"])
    for n in names:
        for c in C:
            r = recs[n][c]; tb = r["total_bytes"]
            w.writerow([n, c, r["valid"], tb, r["raw_bytes"], f'{r["raw_bytes"]/tb:.4f}' if tb else "", f'{r.get("enc_ms",0):.1f}', f'{r.get("dec_ms",0):.1f}', r.get("file_sha256", "")])
P("# ELLIE (3-stage, profile 2) vs JPEG XL and PNG: 50-image corpus\n")
P(f"Images: {len(names)}; valid (decoded, pixel-identical, spec-identical) for all codecs: {len(ok)}; invalid records: {sum(1 for n in names for c in C if not recs[n][c].get('valid'))}.\n")
P(f"Raw (1024x1024 centre crop, RGB8): {raw} B, {px} px. Ratio = raw / stored for every codec.\n")
P("## Aggregate (sum of stored bytes)\n")
rows = []
E = tot("ellie", "total_bytes")
for c in C:
    t = tot(c, "total_bytes")
    rows.append([c, t, f"{t*8/px:.3f}", f"{raw/t:.3f}", "0.0%" if c == "ellie" else f"{(E/t-1)*100:+.2f}%"])
P(table(["codec", "total bytes", "bpp", "aggregate ratio", "ELLIE size vs codec"], rows)); P("\n(Negative = ELLIE smaller.)\n")
P("## Timing (sum of best-of-2, ms; net = minus per-call process floor for ffmpeg codecs; ELLIE/PNG in-process)\n")
rows = []
for c in C:
    et, dt = tot(c, "enc_ms"), tot(c, "dec_ms")
    fe, fd = (calib[c][0] * len(ok), calib[c][1] * len(ok)) if c.startswith("jxl") else (0, 0)
    en, dn = et - fe, dt - fd
    rows.append([c, f"{et:.0f}", f"{en:.0f}", f"{px/en/1e3:.2f}", f"{en/len(ok):.0f}", f"{dt:.0f}", f"{dn:.0f}", f"{px/dn/1e3:.2f}", f"{dn/len(ok):.0f}"])
P(table(["codec", "enc total", "enc net", "enc MP/s net", "enc ms/img", "dec total", "dec net", "dec MP/s net", "dec ms/img"], rows)); P("")
P("## Per-image (bytes, ratio, ELLIE vs codec; ms enc/dec)\n")
rows = []
for n in names:
    r = recs[n]; e = r["ellie"]["total_bytes"]
    rows.append([n] + [f'{r[c]["total_bytes"]} ({r[c]["raw_bytes"]/r[c]["total_bytes"]:.2f})' for c in C] + [f'{(e/r["jxl-e7-t1"]["total_bytes"]-1)*100:+.1f}%', f'{(e/r["jxl-e9-t1"]["total_bytes"]-1)*100:+.1f}%', f'{(e/r["png-rs"]["total_bytes"]-1)*100:+.1f}%']
                + [f'{r[c]["enc_ms"]:.0f}/{r[c]["dec_ms"]:.0f}' for c in C])
P(table(["image", "ellie B (ratio)", "jxl-e7 B", "jxl-e9 B", "png B", "vs e7", "vs e9", "vs png", "ellie ms e/d", "e7 ms e/d", "e9 ms e/d", "png ms e/d"], rows)); P("")
for c in []:
    w = sum(1 for n in ok if recs[n]["ellie"]["total_bytes"] < recs[n][c]["total_bytes"])
    P(f"- ELLIE smaller than {c} on {w}/{len(ok)} images.")
open(os.path.join(d, "report.md"), "w", encoding="utf-8").write("\n".join(o) + "\n")
