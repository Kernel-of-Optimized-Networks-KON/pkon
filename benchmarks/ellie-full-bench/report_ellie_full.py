"""Aggregate bench_ellie_full results.json -> per_image.csv + report.md (same conventions as benchmarks/pkon2/report50.py:
aggregate ratio from summed bytes; timing = sum of best-of-2 per image, `net` subtracts the per-call process floor
measured on the 33x17 calibration image; MP/s = total pixels / total time)."""
import csv, json, sys, os, statistics as st

d = sys.argv[1]
R = json.load(open(os.path.join(d, "results.json"), encoding="utf-8"))
calib = {c: (e, de) for c, e, de in R["calibration_ms"]}
man = {r["name"]: r for r in csv.DictReader(open("benchmarks/pkon2/20261027-155433Z-bench50/dataset_manifest.csv", encoding="utf-8"))}
CODECS = ["ellie", "jxl-e7-t1", "jxl-e9-t1", "webp-m6-t1", "png-rs"]
recs = {}
for r in R["records"]:
    recs.setdefault(r["image"], {})[r["codec"]] = r
names = [i["name"] for i in R["images"]]
out = []
P = out.append

# crop identity vs the historical bench50 manifest
crop_ok = sum(1 for i in R["images"] if i["crop_sha256"] == man[i["name"]]["crop_sha256"])
valid_imgs = [n for n in names if all(recs[n][c].get("valid") for c in CODECS)]
tot = lambda c, k: sum(recs[n][c][k] for n in valid_imgs)
raw = sum(recs[n]["ellie"]["raw_bytes"] for n in valid_imgs)
px = sum(recs[n]["ellie"]["pixels"] for n in valid_imgs)


def table(h, rows):
    return "\n".join(["| " + " | ".join(h) + " |", "|" + "---|" * len(h)] + ["| " + " | ".join(map(str, r)) + " |" for r in rows])


with open(os.path.join(d, "per_image.csv"), "w", newline="") as f:
    w = csv.writer(f)
    w.writerow(["image", "category", "codec", "valid", "byte_exact", "total_bytes", "raw_bytes", "ratio", "bpp", "enc_ms", "dec_ms", "enc_MBps_raw", "dec_MBps_raw", "file_sha256", "note"])
    for n in names:
        cat = man[n]["category"]
        for c in CODECS:
            r = recs[n][c]
            tb = r["total_bytes"]
            w.writerow([n, cat, c, r["valid"], r["byte_exact"], tb, r["raw_bytes"], f'{r["raw_bytes"]/tb:.4f}' if tb else "", f'{tb*8/r["pixels"]:.4f}' if tb else "",
                        f'{r.get("enc_ms", 0):.1f}', f'{r.get("dec_ms", 0):.1f}', f'{r["raw_bytes"]/r["enc_ms"]/1e3:.2f}' if r.get("enc_ms") else "", f'{r["raw_bytes"]/r["dec_ms"]/1e3:.2f}' if r.get("dec_ms") else "", r.get("file_sha256", ""), r.get("note", "")])

P("# ELLIE vs the established lossless benchmark (bench50 corpus, bench50 methodology)\n")
P(f"Images: {len(names)}; valid for all five codecs: {len(valid_imgs)}; invalid/failed records: {sum(1 for r in R['records'] if not r.get('valid'))}. "
  f"Crops byte-identical (sha256) to the historical bench50 manifest: {crop_ok}/{len(names)}.\n")
P("**Raster / definitions (unchanged from `benchmarks/pkon2/20261027-155433Z-bench50/report.md`):** decoded pixels of each photo, 1024x1024 centre crop, RGB8 (JPEG sources: lossless coding of the decoded raster, not the JPEG bytes). "
  "Ratio = raw pixel bytes / stored bytes, same buffer for every codec. Sizes are the complete file/stream each codec emits: JXL bare codestream, WebP RIFF/VP8L file, PNG file, "
  "**ELLIE = headerless Stage 4 payload** (w/h/layout out-of-band, as for earlier PKON2 reports; a PKON v2 container would add 24 B/image, = "
  f"{24*len(valid_imgs)} B total, shown below). Competitors/configs: jxl-e7-t1, jxl-e9-t1 (ffmpeg libjxl `-distance 0 -threads 1`), webp-m6-t1 (ffmpeg libwebp lossless, method 6), png-rs (Rust `png`, Best + Adaptive). "
  "Every output decoded from its own bytes and compared byte-for-byte with the canonical buffer; ELLIE additionally re-encoded and compared (determinism). "
  "Timing: best of 2 per image/codec; ELLIE and png-rs in-process, ffmpeg codecs include process spawn + temp-file I/O (`net` subtracts the per-call floor). ELLIE timing includes plane split/interleave and codec setup. Single-threaded ELLIE; 8-logical-CPU Windows 11 machine, release build.\n")

# aggregate
P("## Aggregate compression (sum of stored bytes)\n")
P(f"Raw: {raw} B over {len(valid_imgs)} images ({px} px).\n")
base = tot("ellie", "total_bytes")
rows = []
for c in CODECS:
    b = tot(c, "total_bytes")
    rat = [recs[n][c]["raw_bytes"] / recs[n][c]["total_bytes"] for n in valid_imgs]
    rows.append([c, b, f"{b*8/px:.3f}", f"{raw/b:.3f}", f"{(base/b-1)*100:+.1f}%", f"{st.mean(rat):.3f}", f"{st.median(rat):.3f}"])
P(table(["codec", "total bytes", "bpp (24 raw)", "aggregate ratio", "ELLIE size vs codec", "mean per-image ratio", "median per-image ratio"], rows))
P(f"\n(`ELLIE size vs codec` = ELLIE bytes / codec bytes - 1; positive means ELLIE is larger. With a 24 B PKON v2 header ELLIE would be {base+24*len(valid_imgs)} B, ratio {raw/(base+24*len(valid_imgs)):.3f}.)\n")

# timing
P("## Timing (sum over images of best-of-2, ms)\n")
rows = []
tim = {}
for c in CODECS:
    e = sum(recs[n][c]["enc_ms"] for n in valid_imgs)
    de = sum(recs[n][c]["dec_ms"] for n in valid_imgs)
    en = e - calib[c][0] * len(valid_imgs)
    dn = de - calib[c][1] * len(valid_imgs)
    tim[c] = (en, dn)
    rows.append([c, f"{e:.0f}", f"{en:.0f}", f"{px/1e6/(en/1e3):.2f}", f"{raw/1e6/(en/1e3):.2f}", f"{de:.0f}", f"{dn:.0f}", f"{px/1e6/(dn/1e3):.2f}", f"{raw/1e6/(dn/1e3):.2f}", f"{en/len(valid_imgs):.0f}", f"{dn/len(valid_imgs):.0f}"])
P(table(["codec", "enc total", "enc net", "enc MP/s net", "enc MB/s net (raw)", "dec total", "dec net", "dec MP/s net", "dec MB/s net (raw)", "enc net ms/img", "dec net ms/img"], rows))
P("\nPeak memory: the established benchmark does not measure it, so it is not measured here.\n")

# rankings
P("## Rankings\n")
P("### Per-image size rank (1 = smallest output); count of images at each rank\n")
rk = {c: [0] * 5 for c in CODECS}
smallest = {c: 0 for c in CODECS}
for n in valid_imgs:
    order = sorted(CODECS, key=lambda c: recs[n][c]["total_bytes"])
    for i, c in enumerate(order):
        rk[c][i] += 1
P(table(["codec", "rank1", "rank2", "rank3", "rank4", "rank5", "mean rank"], [[c] + rk[c] + [f"{sum((i+1)*v for i,v in enumerate(rk[c]))/len(valid_imgs):.2f}"] for c in CODECS]))
P("\n### Aggregate ranks\n")
sz = sorted(CODECS, key=lambda c: tot(c, "total_bytes"))
P("- Compression (aggregate bytes, best first): " + " > ".join(sz))
P("- Encode speed (net MP/s, fastest first): " + " > ".join(sorted(CODECS, key=lambda c: tim[c][0])))
P("- Decode speed (net MP/s, fastest first): " + " > ".join(sorted(CODECS, key=lambda c: tim[c][1])) + "\n")
P("### Per-image encode / decode speed rank (1 = fastest, net of process floor); count of images\n")
for kind, key, ci in (("encode", "enc_ms", 0), ("decode", "dec_ms", 1)):
    rr = {c: [0] * 5 for c in CODECS}
    for n in valid_imgs:
        order = sorted(CODECS, key=lambda c: recs[n][c][key] - calib[c][ci])
        for i, c in enumerate(order):
            rr[c][i] += 1
    P(f"{kind}:\n")
    P(table(["codec", "rank1", "rank2", "rank3", "rank4", "rank5"], [[c] + rr[c] for c in CODECS]))
    P("")

# head to head
P("## ELLIE vs each competitor\n")
rows = []
for c in CODECS[1:]:
    ds = [recs[n]["ellie"]["total_bytes"] / recs[n][c]["total_bytes"] - 1 for n in valid_imgs]
    wins = sum(1 for x in ds if x < 0)
    rows.append([c, f"{(base/tot(c,'total_bytes')-1)*100:+.1f}%", f"{min(ds)*100:+.1f}% / {st.median(ds)*100:+.1f}% / {max(ds)*100:+.1f}%", f"{wins}/{len(valid_imgs)}",
                 f"{tim[c][0]/tim['ellie'][0]:.2f}x", f"{tim[c][1]/tim['ellie'][1]:.2f}x"])
P(table(["competitor", "ELLIE agg. size vs it", "per-image min / median / max", "images ELLIE smaller", "its net enc time / ELLIE's", "its net dec time / ELLIE's"], rows))
P("\n(time ratio > 1 means the competitor is slower than ELLIE; < 1 means faster.)\n")

# by category
P("## Per-image results\n")
hdr = ["image", "cat", "raw B"] + [f"{c} B" for c in CODECS] + ["ELLIE ratio", "best", "ELLIE enc/dec ms"]
rows = []
for n in names:
    r = recs[n]
    best = min(CODECS, key=lambda c: r[c]["total_bytes"])
    rows.append([n, man[n]["category"], r["ellie"]["raw_bytes"]] + [r[c]["total_bytes"] for c in CODECS] + [f'{r["ellie"]["raw_bytes"]/r["ellie"]["total_bytes"]:.3f}', best, f'{r["ellie"]["enc_ms"]:.0f}/{r["ellie"]["dec_ms"]:.0f}'])
P(table(hdr, rows))

# ELLIE side
side = [recs[n]["ellie"].get("ellie_side") for n in valid_imgs]
if all(side):
    ideal = sum(s["ideal_stage3_bytes"] for s in side)
    carry = sum(s["carry_on_bytes"] for s in side)
    model_ms = sum(s["model_only_enc_ms"] for s in side)
    P("\n## ELLIE-only measurements (not part of any cross-codec comparison)\n")
    P(f"- Modelled cost: ideal -log2(p) Stage 3 information {ideal:.0f} B vs Stage 4 actual {base} B -> Stage 4 overhead {(base/ideal-1)*100:.4f}%.")
    P(f"- carry_model=true (Stage 3 state continues across planes): {carry} B vs {base} B ({(carry/base-1)*100:+.3f}%). Cross-codec table uses the default (carry off).")
    P(f"- Model-only (Stages 1-3, no coder) encode time: {model_ms:.0f} ms total vs full encode {sum(recs[n]['ellie']['enc_ms'] for n in valid_imgs):.0f} ms.")

P("\n## Correctness\n")
bad = [(n, c) for n in names for c in CODECS if not recs[n][c].get("valid")]
P(f"- Records valid & byte-exact: {sum(1 for r in R['records'] if r.get('valid'))}/{len(R['records'])}. Failures: {bad if bad else 'none'}.")
P(f"- ELLIE pixel-identical on {sum(1 for n in names if recs[n]['ellie']['byte_exact'])}/{len(names)} images; deterministic re-encode on all valid images (nondeterminism would invalidate the record).")
P(f"- Truncation (half stream) rejected or mismatching for every codec on the calibration image (preflight, see stdout.txt).")
open(os.path.join(d, "report.md"), "w", encoding="utf-8").write("\n".join(out) + "\n")
print("\n".join(out))
