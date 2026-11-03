# ULISA codec 7 vs ELLIE, LISA, JPEG XL and PNG: full-corpus lossless RAW benchmark

Written for: the ULISA project owner (decision-grade evidence; no algorithm changes were made).

## Scope and deviations (read first)

- **This is not the RGB8 50-image corpus.** Codec 7 is a Bayer RAW codec (`pkon::encode_ulisa`; input = u16 CFA mosaic + metadata) and cannot encode the RGB8/RGBA8 bench50 crops. By owner decision the benchmark uses the frozen **25-image u16 RAW corpus** (`data/praw1/canon`, `manifest.json`; the sha256 of every source was verified; split in `split.json`). It is 25 full-size rasters, not 50 crops.
- **Frozen candidate:** commit `10d67a87630e3db8a7505fdba322d2bd1544cf68` (main). `src/` is unmodified (hash in `environment.txt`). The only tracked change is a `[[bin]]` entry in `Cargo.toml` for the new harness `benchmarks/ulisa-codec7-corpus/bench_ulisa_codec7_corpus.rs` (uncommitted). The harness binary's sha256 is in `environment.txt`.
- **Held-out images:** the original 8-image test split was never used to develop ULISA. 7 of them are in the aggregates (the 8th, `nikon_d80_piano`, has no ELLIE result). Train+val and held-out are reported separately.
- **Coverage of the 25 images:**
  - **23 images** have an exact result for every codec; all aggregate and pairwise numbers use these.
  - **`nikon_d80_piano`** (3900x2611, odd height): the four parity planes differ in size, so the ELLIE arm (planes packed as one RGBA image, as in the earlier 3-image bench) cannot be built: **N/A**. The first run panicked on a harness assert here; I replaced the assert with an N/A and resumed with the remaining four images (the 21 earlier records are untouched). LISA, ULISA, JXL, PNG are all exact on it. ULISA 7,056,813 B: -4.85% vs LISA, -15.64% vs JXL e7, -14.68% vs JXL e9, -41.58% vs PNG.
  - **`x100s_xtrans_chart`** (X-Trans): LISA, ELLIE and ULISA reject it (not a 2x2 Bayer frame): N/A, not a loss. JXL and PNG ran exactly but are not compared.
- **Inputs per codec:** LISA = `pkon::encode_raw`; ULISA = `pkon::encode_ulisa_report` (codec id 7 verified in header); ELLIE = LISA-frame parity planes as 16-bit RGBA through `encode_standard_with(Untransformed)` (same arm as the 3-image bench; encode time includes frame build and packing); all full `.pkon` files. JXL = ffmpeg 9.0.2 libjxl `-distance 0 -effort 7|9 -threads 1`, gray16le. PNG = Rust png 0.18.1 via image 0.25.10, Best + Adaptive, Gray 16-bit.
- **JXL container:** ffmpeg wrapped the 16-bit gray output in a 49-byte ISOBMFF container. Stored files and all sizes are the **bare codestream** (container stripped), and decoding used the stripped files.
- **bpp** is bits per sensor sample (one 16-bit container value per pixel), not per RGB pixel.
- **Timing:** best of 2 per image/codec/direction, sequential, single-threaded, release build, on a laptop (i7-1185G7, 8 logical CPUs, 16 GB, Windows 11; `environment.txt`). ffmpeg times include process spawn and temp-file I/O (about 0.035 s per call, subtracted in the "net" columns). Timings carry laptop noise; e.g. `parrot_anafi_dusk` PNG encode (17 s) is an outlier, as in the earlier LISA benchmark.

## Correctness

Images attempted: 25. Every produced file was decoded from its own bytes and compared with the source: **0 mismatches, 0 decode failures.** Exact reconstructions: LISA 24/24 attempted, ELLIE 23/23 attempted, ULISA 24/24 attempted, JXL e7 25/25, JXL e9 25/25, PNG 25/25. The only non-results are the N/A cases above (LISA/ELLIE/ULISA on X-Trans; ELLIE on the odd-height Nikon). Raw size of the 23-image aggregate: 816,424,128 B (408,212,064 samples). Every number traces to a file in `files/` (sha256 in `per_image.csv`).

## Headline

**ULISA is the smallest codec overall, and it is smaller than LISA, JXL e7, JXL e9 and PNG on all 23 images.** It beats ELLIE on 19 of 23. Aggregate order: ULISA 338.19 MB < ELLIE 342.42 MB < LISA 355.99 MB < JXL e9 403.97 MB < JXL e7 414.18 MB < PNG 557.35 MB.

## Answers

1. **vs JXL effort 7:** ULISA is **18.35% smaller** in aggregate (median per image -18.85%, range -6.2% to -30.4%), smaller on 23/23. Held-out: -21.69%; train+val: -17.33%.
2. **vs JXL effort 9:** **16.28% smaller** (median -17.48%, range -4.85% to -27.23%), 23/23. Held-out -19.83%; train+val -15.20%. Effort 9 closes only about 2 points of the e7 gap.
3. **vs PNG** (16-bit gray, Best + Adaptive): **39.32% smaller** (median -41.16%, range -26.8% to -54.5%), 23/23.
4. **ULISA vs ELLIE:** aggregate **-1.24%** (4.23 MB of 342.4 MB); median per image -0.75%. Train+val -1.25%, held-out -1.19%. This is the net effect of the flank-context integration (plus ULISA container fields) over ELLIE on the same planes: small, but the same size on development and held-out sets. Versus LISA, ULISA is -5.00% and ELLIE alone is -3.8%, so roughly three quarters of ULISA's gain over LISA is already in ELLIE.
5. **Wins vs ELLIE: 19 of 23** (train+val 13/16, held-out 6/7), no ties. The 4 losses (`pentax_kx_lichen` +0.03%, `pentax_k70_bulb` +0.06%, `leica_x2_lowlight` +0.03%, `sigma_fp_bike14` +0.03%) are all within 0.06%, effectively ties. The largest win is `leica_m8_house` (8-bit) at -7.13%; the next largest is -2.35%.
6. **Consistency across image types:** against the outside codecs, yes. ULISA beats JXL e9 and PNG on every image, at every bit depth (8/10/12/14/16) and in every sensor family. Its margin over JXL e9 is smaller on 14-bit (-15.3%) and 16-bit (-14.0%) and on the highest-detail images (`pentax_k70_bulb` -4.85%), and larger on 8-bit (-21.3%) and clean or low-light images (`sigma_fp_bike8` -27.2%). The gain over ELLIE is not uniform: it is concentrated in Leica (-2.3%), phones (-1.8%), 8-bit (-4.8%) and 10-bit (-1.5%), and near zero on Pentax (-0.58%, 3/5 wins), Sigma fp (-0.01%, 1/2), Samsung and drones. Groups have 1-10 images each, so family-level numbers are indicative only. L1 activated on only 2 images (`dji_fc220_dusk`, `parrot_anafi_dusk`); the flank context was non-neutral on 61-75% of coded samples.
7. **Speed (aggregate, 23 images, net, single-thread):**
   - Encode, Mpx/s: PNG 6.04 > ELLIE 4.49 > ULISA 3.81 > JXL e7 2.66 > LISA 1.23 > JXL e9 0.55.
   - Decode, Mpx/s: PNG 104 > JXL e7 6.31 > JXL e9 5.91 > ELLIE 4.02 > ULISA 3.29 > LISA 1.24.
   - ULISA vs ELLIE: 17.7% slower to encode (107.1 s vs 91.0 s) and 22.3% slower to decode (124.1 s vs 101.5 s).
   - ULISA vs LISA: 3.1x faster to encode, 2.7x faster to decode.
   - ULISA vs JXL e7: encodes 1.43x faster, decodes 1.9x slower. Vs JXL e9: encodes 6.9x faster, decodes 1.8x slower.
   - Decode is the weak point of the context-modelling codecs relative to JXL and PNG.

## Interpretation

On this corpus ULISA's size advantage over general-purpose lossless codecs is large and uniform. Its advantage over ELLIE is real but small (about 1.2%), and it costs roughly 18-22% more time. The held-out result (-1.19%, 6/7) matches train+val (-1.25%, 13/16), so there is no sign of overfitting to the development images. A ~1% gain with four near-zero losses is a modest effect. Caveats: one corpus of 23 comparable images, one camera per family in most cases, and laptop timings. No algorithm changes were made in response to any result.

## Files

`manifest.json`, `split.json`, `per_image.csv` (bytes, bpp, ratio, sha256, file path per codec/image), `timings.csv`, `results.json` (aggregates, pairs, all records), `results.jsonl` (raw harness output), `run.log`, `environment.txt`, `files/` (146 encoded files, 2.4 GB), `report_data.md` (the generated tables, also appended below). Harness and report script: `benchmarks/ulisa-codec7-corpus/`.

## Data tables (generated by report.py)

### all_valid: 23 images, 408,212,064 px, 816,424,128 raw bytes

| codec | total bytes | bpp (of 16-bit container pixel) | ratio | ULISA size vs codec | exact |
|---|---|---|---|---|---|
| LISA (4) | 355,991,419 | 6.9766 | 2.2934 | -5.00% | 23/23 |
| ELLIE (6) | 342,420,617 | 6.7106 | 2.3843 | -1.24% | 23/23 |
| ULISA (7) | 338,186,389 | 6.6277 | 2.4141 | - | 23/23 |
| JXL e7 | 414,184,074 | 8.1170 | 1.9712 | -18.35% | 23/23 |
| JXL e9 | 403,972,838 | 7.9169 | 2.0210 | -16.28% | 23/23 |
| PNG | 557,351,642 | 10.9228 | 1.4648 | -39.32% | 23/23 |

### train+val: 16 images, 302,950,848 px, 605,901,696 raw bytes

| codec | total bytes | bpp (of 16-bit container pixel) | ratio | ULISA size vs codec | exact |
|---|---|---|---|---|---|
| LISA (4) | 275,889,189 | 7.2854 | 2.1962 | -4.85% | 16/16 |
| ELLIE (6) | 265,829,558 | 7.0197 | 2.2793 | -1.25% | 16/16 |
| ULISA (7) | 262,504,048 | 6.9319 | 2.3082 | - | 16/16 |
| JXL e7 | 317,542,363 | 8.3853 | 1.9081 | -17.33% | 16/16 |
| JXL e9 | 309,566,156 | 8.1747 | 1.9573 | -15.20% | 16/16 |
| PNG | 421,463,022 | 11.1295 | 1.4376 | -37.72% | 16/16 |

### test_heldout: 7 images, 105,261,216 px, 210,522,432 raw bytes

| codec | total bytes | bpp (of 16-bit container pixel) | ratio | ULISA size vs codec | exact |
|---|---|---|---|---|---|
| LISA (4) | 80,102,230 | 6.0879 | 2.6282 | -5.52% | 7/7 |
| ELLIE (6) | 76,591,059 | 5.8210 | 2.7487 | -1.19% | 7/7 |
| ULISA (7) | 75,682,341 | 5.7520 | 2.7817 | - | 7/7 |
| JXL e7 | 96,641,711 | 7.3449 | 2.1784 | -21.69% | 7/7 |
| JXL e9 | 94,406,682 | 7.1750 | 2.2300 | -19.83% | 7/7 |
| PNG | 135,888,620 | 10.3277 | 1.5492 | -44.31% | 7/7 |

### Timing, all valid images (sum of best-of-2 seconds; net = minus ffmpeg per-call floor)

| codec | enc s | enc net s | enc Mpx/s | dec s | dec net s | dec Mpx/s |
|---|---|---|---|---|---|---|
| LISA (4) | 332.5 | 332.5 | 1.228 | 330.6 | 330.6 | 1.235 |
| ELLIE (6) | 91.0 | 91.0 | 4.487 | 101.5 | 101.5 | 4.021 |
| ULISA (7) | 107.1 | 107.1 | 3.810 | 124.1 | 124.1 | 3.289 |
| JXL e7 | 154.5 | 153.6 | 2.657 | 65.6 | 64.6 | 6.314 |
| JXL e9 | 743.0 | 742.1 | 0.550 | 70.1 | 69.1 | 5.909 |
| PNG | 67.6 | 67.6 | 6.040 | 3.9 | 3.9 | 104.020 |

### Pairwise wins/losses by file size (negative % = first codec smaller)

| pair | set | wins/ties/losses | median | mean | best | worst |
|---|---|---|---|---|---|---|
| ULISA (7) vs ELLIE (6) | all_valid | 19/0/4 | -0.75% | -1.24% | -7.13% | +0.06% |
| ULISA (7) vs ELLIE (6) | train+val | 13/0/3 | -1.34% | -1.45% | -7.13% | +0.06% |
| ULISA (7) vs ELLIE (6) | test_heldout | 6/0/1 | -0.39% | -0.77% | -2.33% | +0.03% |
| ULISA (7) vs LISA (4) | all_valid | 23/0/0 | -5.09% | -5.52% | -12.51% | -3.18% |
| ULISA (7) vs LISA (4) | train+val | 16/0/0 | -5.02% | -5.36% | -12.51% | -3.18% |
| ULISA (7) vs LISA (4) | test_heldout | 7/0/0 | -5.19% | -5.87% | -8.37% | -4.17% |
| ULISA (7) vs JXL e7 | all_valid | 23/0/0 | -18.85% | -18.93% | -30.40% | -6.22% |
| ULISA (7) vs JXL e7 | train+val | 16/0/0 | -17.67% | -17.15% | -22.83% | -6.22% |
| ULISA (7) vs JXL e7 | test_heldout | 7/0/0 | -22.34% | -23.01% | -30.40% | -14.87% |
| ULISA (7) vs JXL e9 | all_valid | 23/0/0 | -17.48% | -17.12% | -27.23% | -4.85% |
| ULISA (7) vs JXL e9 | train+val | 16/0/0 | -15.54% | -15.35% | -20.79% | -4.85% |
| ULISA (7) vs JXL e9 | test_heldout | 7/0/0 | -21.05% | -21.15% | -27.23% | -13.72% |
| ULISA (7) vs PNG | all_valid | 23/0/0 | -41.16% | -41.32% | -54.49% | -26.82% |
| ULISA (7) vs PNG | train+val | 16/0/0 | -39.45% | -38.89% | -47.49% | -26.82% |
| ULISA (7) vs PNG | test_heldout | 7/0/0 | -49.00% | -46.87% | -54.49% | -33.00% |
| ELLIE (6) vs LISA (4) | all_valid | 23/0/0 | -3.83% | -4.33% | -8.01% | -2.54% |
| ELLIE (6) vs LISA (4) | train+val | 16/0/0 | -3.66% | -3.97% | -5.79% | -2.54% |
| ELLIE (6) vs LISA (4) | test_heldout | 7/0/0 | -4.92% | -5.14% | -8.01% | -2.92% |
| ELLIE (6) vs JXL e9 | all_valid | 23/0/0 | -14.53% | -16.05% | -26.95% | -4.91% |
| ELLIE (6) vs JXL e9 | train+val | 16/0/0 | -14.00% | -14.10% | -19.66% | -4.91% |
| ELLIE (6) vs JXL e9 | test_heldout | 7/0/0 | -19.53% | -20.52% | -26.95% | -13.61% |

### Per image (bytes; ULISA % vs each; split)

| image | split | WxH | bits | LISA (4) | ELLIE (6) | ULISA (7) | JXL e7 | JXL e9 | PNG | U vs ELLIE | U vs LISA | U vs e7 | U vs e9 | U vs PNG |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| pentax_k1ii_station | train | 7392x4950 | 14 | 36,498,518 | 35,385,681 | 34,932,961 | 45,265,732 | 43,455,404 | 58,393,598 | -1.28% | -4.29% | -22.83% | -19.61% | -40.18% |
| pentax_k30_frog | test | 4960x3300 | 12 | 11,836,280 | 11,274,986 | 11,224,464 | 15,314,064 | 14,885,894 | 22,007,041 | -0.45% | -5.17% | -26.70% | -24.60% | -49.00% |
| pentax_kx_lichen | val | 4352x2868 | 12 | 11,458,135 | 11,055,845 | 11,059,407 | 12,647,453 | 12,614,008 | 17,332,232 | +0.03% | -3.48% | -12.56% | -12.32% | -36.19% |
| pentax_q7_cat | test | 4064x3000 | 12 | 9,288,997 | 8,831,931 | 8,820,890 | 10,362,010 | 10,223,095 | 15,015,542 | -0.13% | -5.04% | -14.87% | -13.72% | -41.25% |
| pentax_k70_bulb | train | 6080x4064 | 14 | 20,396,919 | 19,615,755 | 19,626,714 | 20,929,119 | 20,627,807 | 27,624,796 | +0.06% | -3.78% | -6.22% | -4.85% | -28.95% |
| leica_cl_chateau | val | 6120x4016 | 14 | 25,248,992 | 24,456,683 | 23,963,809 | 30,916,839 | 29,835,762 | 39,101,664 | -2.02% | -5.09% | -22.49% | -19.68% | -38.71% |
| leica_m10_brick | test | 5984x3992 | 14 | 27,738,823 | 26,927,464 | 26,298,847 | 32,213,338 | 31,392,085 | 39,249,565 | -2.33% | -5.19% | -18.36% | -16.22% | -33.00% |
| leica_q_canoe | train | 6120x4016 | 14 | 27,966,357 | 26,862,186 | 26,231,450 | 30,549,389 | 30,036,632 | 39,167,918 | -2.35% | -6.20% | -14.13% | -12.67% | -33.03% |
| leica_x2_lowlight | test | 4944x3272 | 12 | 8,727,995 | 8,182,983 | 8,185,126 | 11,475,902 | 11,167,281 | 17,984,990 | +0.03% | -6.22% | -28.68% | -26.70% | -54.49% |
| leica_m8_house | train | 3920x2638 | 8 | 5,443,197 | 5,127,830 | 4,762,007 | 5,929,220 | 5,770,871 | 8,283,750 | -7.13% | -12.51% | -19.69% | -17.48% | -42.51% |
| pixel6pro_valley | train | 4080x3072 | 14 | 11,299,938 | 10,880,600 | 10,636,464 | 12,953,370 | 12,730,252 | 17,964,797 | -2.24% | -5.87% | -17.89% | -16.45% | -40.79% |
| dji_fc220_dusk | test | 4000x3000 | 14 | 9,006,259 | 8,651,334 | 8,630,889 | 10,755,315 | 10,591,873 | 15,626,058 | -0.24% | -4.17% | -19.75% | -18.51% | -44.77% |
| parrot_anafi_dusk | train | 5344x4016 | 14 | 12,291,116 | 11,582,188 | 11,569,302 | 13,794,888 | 13,551,367 | 20,381,694 | -0.11% | -5.87% | -16.13% | -14.63% | -43.24% |
| hasselblad_tallinn | train | 5568x3648 | 16 | 25,974,595 | 25,314,340 | 25,149,098 | 30,468,723 | 29,253,760 | 34,366,570 | -0.65% | -3.18% | -17.46% | -14.03% | -26.82% |
| sigma_fp_bike14 | val | 6064x4042 | 14 | 23,748,198 | 22,960,983 | 22,967,771 | 27,155,081 | 26,489,284 | 37,238,402 | +0.03% | -3.29% | -15.42% | -13.29% | -38.32% |
| sigma_fp_bike8 | test | 3856x2170 | 8 | 2,904,029 | 2,671,423 | 2,661,037 | 3,823,192 | 3,656,935 | 5,847,101 | -0.39% | -8.37% | -30.40% | -27.23% | -54.49% |
| nexus6_courtyard | val | 4208x3120 | 10 | 8,006,715 | 7,577,802 | 7,521,180 | 9,268,678 | 9,148,921 | 13,598,778 | -0.75% | -6.06% | -18.85% | -17.79% | -44.69% |
| lg_h815_mountain | train | 5312x2988 | 10 | 9,588,249 | 9,071,844 | 8,871,509 | 11,177,070 | 10,968,079 | 16,896,412 | -2.21% | -7.48% | -20.63% | -19.12% | -47.49% |
| ricoh_gr_wood | test | 4960x3280 | 12 | 10,599,847 | 10,050,938 | 9,861,088 | 12,697,890 | 12,489,519 | 20,158,323 | -1.89% | -6.97% | -22.34% | -21.05% | -51.08% |
| ricoh_gr3_dunes | train | 6112x4064 | 14 | 26,829,161 | 25,929,813 | 25,473,025 | 30,366,859 | 29,592,656 | 39,707,657 | -1.76% | -5.05% | -16.12% | -13.92% | -35.85% |
| samsung_gx20_berries | train | 4864x3136 | 12 | 11,666,928 | 11,229,394 | 11,225,784 | 14,100,783 | 13,701,445 | 20,970,661 | -0.03% | -3.78% | -20.39% | -18.07% | -46.47% |
| bmpcc4k_person | train | 4128x2176 | 12 | 7,517,952 | 7,256,602 | 7,154,499 | 9,161,507 | 9,032,024 | 12,159,423 | -1.41% | -4.83% | -21.91% | -20.79% | -41.16% |
| canon_sx40_snow | train | 4176x3060 | 12 | 11,954,219 | 11,522,012 | 11,359,068 | 12,857,652 | 12,757,884 | 18,274,670 | -1.41% | -4.98% | -11.66% | -10.96% | -37.84% |

### Per image timing (best-of-2 seconds enc/dec)

| image | LISA (4) | ELLIE (6) | ULISA (7) | JXL e7 | JXL e9 | PNG |
|---|---|---|---|---|---|---|
| pentax_k1ii_station | 35.14/32.35 | 9.43/9.27 | 11.20/11.99 | 12.40/7.02 | 78.94/5.46 | 3.37/0.44 |
| pentax_k30_frog | 15.11/14.17 | 2.30/4.58 | 4.49/5.55 | 4.71/1.85 | 26.06/2.08 | 1.76/0.15 |
| pentax_kx_lichen | 10.38/12.70 | 3.71/3.90 | 4.10/4.81 | 4.39/1.48 | 24.36/1.65 | 1.98/0.16 |
| pentax_q7_cat | 12.05/15.30 | 3.14/3.70 | 3.84/4.48 | 7.16/1.91 | 27.49/1.82 | 2.48/0.10 |
| pentax_k70_bulb | 29.12/23.44 | 6.83/6.90 | 6.73/7.84 | 12.41/4.48 | 42.85/5.41 | 4.68/0.23 |
| leica_cl_chateau | 22.36/20.50 | 6.81/7.38 | 7.77/9.51 | 6.21/3.22 | 50.63/4.68 | 2.32/0.31 |
| leica_m10_brick | 19.35/19.85 | 7.14/8.26 | 7.96/10.99 | 7.43/3.70 | 47.04/4.56 | 2.26/0.25 |
| leica_q_canoe | 19.09/20.77 | 7.17/7.73 | 8.53/10.05 | 13.76/5.72 | 80.11/5.56 | 3.10/0.30 |
| leica_x2_lowlight | 17.17/16.79 | 3.70/4.00 | 3.96/4.46 | 6.01/2.61 | 24.04/2.70 | 2.30/0.12 |
| leica_m8_house | 6.86/6.78 | 1.36/1.46 | 1.52/1.77 | 3.89/1.63 | 14.08/1.59 | 3.12/0.05 |
| pixel6pro_valley | 10.54/10.24 | 2.77/3.19 | 4.05/4.57 | 7.49/2.69 | 23.80/2.08 | 1.30/0.12 |
| dji_fc220_dusk | 9.46/9.40 | 2.15/2.24 | 2.42/2.71 | 4.16/1.69 | 16.84/1.73 | 1.57/0.10 |
| parrot_anafi_dusk | 14.90/15.90 | 3.23/3.57 | 3.87/4.45 | 7.83/3.37 | 26.50/4.21 | 17.23/0.15 |
| hasselblad_tallinn | 12.77/12.80 | 5.46/6.63 | 6.35/6.94 | 4.41/2.47 | 32.41/3.27 | 2.08/0.18 |
| sigma_fp_bike14 | 16.15/15.99 | 4.88/5.47 | 6.03/6.65 | 9.48/3.91 | 40.72/4.04 | 2.25/0.24 |
| sigma_fp_bike8 | 4.11/4.13 | 0.82/0.88 | 0.92/1.10 | 2.55/1.22 | 9.14/1.19 | 2.47/0.05 |
| nexus6_courtyard | 9.44/9.48 | 1.89/2.08 | 2.16/2.57 | 5.26/1.88 | 17.98/2.00 | 2.26/0.10 |
| lg_h815_mountain | 11.09/11.28 | 2.30/2.62 | 2.73/3.12 | 6.25/2.61 | 23.64/2.59 | 3.43/0.11 |
| ricoh_gr_wood | 11.99/11.91 | 2.53/2.82 | 2.93/3.39 | 6.22/2.67 | 23.90/2.69 | 1.80/0.14 |
| ricoh_gr3_dunes | 16.19/16.08 | 5.58/6.22 | 6.54/7.40 | 8.37/3.91 | 47.37/4.45 | 2.15/0.25 |
| samsung_gx20_berries | 12.37/12.27 | 2.71/3.01 | 3.15/3.64 | 6.27/2.36 | 26.26/2.56 | 1.60/0.16 |
| bmpcc4k_person | 8.33/9.77 | 2.59/2.86 | 2.96/2.86 | 2.39/1.23 | 12.18/1.62 | 0.90/0.10 |
| canon_sx40_snow | 8.58/8.67 | 2.50/2.76 | 2.89/3.27 | 5.47/1.99 | 26.64/2.10 | 1.18/0.12 |

### By bit depth / sensor family (ULISA vs ELLIE, vs JXL e9; aggregate bytes)

| grouping | group | n | ULISA vs ELLIE | ULISA beats ELLIE | ULISA vs JXL e9 | ULISA vs PNG |
|---|---|---|---|---|---|---|
| bit depth | 10-bit | 2 | -1.54% | 2/2 | -18.51% | -46.25% |
| bit depth | 12-bit | 8 | -0.65% | 6/8 | -18.56% | -45.18% |
| bit depth | 14-bit | 10 | -1.37% | 8/10 | -15.29% | -37.11% |
| bit depth | 16-bit | 1 | -0.65% | 1/1 | -14.03% | -26.82% |
| bit depth | 8-bit | 2 | -4.82% | 2/2 | -21.26% | -47.47% |
| family | Canon compact | 1 | -1.41% | 1/1 | -10.96% | -37.84% |
| family | Hasselblad MF | 1 | -0.65% | 1/1 | -14.03% | -26.82% |
| family | Leica | 5 | -2.31% | 4/5 | -17.34% | -37.80% |
| family | Pentax APS-C/FF DSLR | 5 | -0.58% | 3/5 | -15.86% | -38.97% |
| family | Ricoh GR | 2 | -1.80% | 2/2 | -16.04% | -40.98% |
| family | Samsung | 1 | -0.03% | 1/1 | -18.07% | -46.47% |
| family | Sigma fp | 2 | -0.01% | 1/2 | -14.99% | -40.52% |
| family | cine | 1 | -1.41% | 1/1 | -20.79% | -41.16% |
| family | drone | 2 | -0.16% | 2/2 | -16.33% | -43.90% |
| family | phone | 3 | -1.82% | 3/3 | -17.71% | -44.22% |

### Images not in the aggregates

- nikon_d80_piano: lisa4: exact=True; ellie6: N/A: unequal parity-plane dimensions cannot be packed as one RGBA image (harness limit); ulisa7: exact=True; jxl_e7: exact=True; jxl_e9: exact=True; png: exact=True
- x100s_xtrans_chart: lisa4: Lisa(InvalidMetadata("Bayer CFA requires a 2x2 repeat")); ellie6: not a supported 2x2 Bayer frame (BayerFrame::build failed); ulisa7: not a supported 2x2 Bayer frame (BayerFrame::build failed); jxl_e7: exact=True; jxl_e9: exact=True; png: exact=True

