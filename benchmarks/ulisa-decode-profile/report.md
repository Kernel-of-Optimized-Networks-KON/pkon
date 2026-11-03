# ULISA codec 7 — decode throughput investigation

Reference: commit `10d67a8` (working tree: only the pre-existing `Cargo.toml` bench entry differs). **No production source file was touched.**
Everything new lives in `benchmarks/ulisa-decode-profile/` (diagnostic crate `harness/`, raw outputs in `raw/`). The corpus files were not re-encoded or modified.

## Bottom line

* Decode is a **single serial chain of ~3 (8-bit, smooth) to ~14 (14-bit RAW) adaptive binary decisions per sample**, each costing ~25–45 ns (≈70–140 cycles) in the mixer + range decoder. Decisions are the unit of cost, not pixels.
* On the 14-bit images that dominate the benchmark, roughly **85–90 % of decode time is the entropy/model loop** (context mixing, slot/weight updates, range decode, and the decode-side dependency). The Stage 1 predictor is ~12 %; merge, CRC, denormalise, allocation and setup together are ~2–3 %.
* The flank (cross-plane) stage is **not** a bottleneck: 0–8 % of model time on the heavy images (within noise).
* Planes **cannot** be decoded in parallel without changing the bitstream (details in §D). Bit-exact optimisations are therefore single-thread micro-architectural work and realistically worth **tens of percent, not 1.9×**. Closing the whole gap to JPEG XL needs fewer or cheaper decisions per sample, which is a format question.
* **Recommended first experiment:** a branch-free range-decoder bit resolution (including the raw p = ½ bits, 21–26 % of all decisions), measured against this report's baseline. Zero bitstream risk. See §Ranking.

## Caveats

1. **Hardware counters unavailable.** No cache-miss or branch-mispredict counts were measured. The mispredict figures below are an *expected-value estimate* from the coded probabilities, not a measurement.
2. **The machine throttles.** Same-process repetitions of one decode varied up to 1.7× (k1ii: 11.2 s, 18.7 s, 16.0 s; the archived benchmark had 11.99 s). First repetitions after idle match the archived numbers. Ablation numbers on `pentax_k1ii_station` (200 s of sustained load) are contaminated by this; `leica_m10_brick` and `sigma_fp_bike8` are less affected but still ±10–30 %. **Treat percentages as ±10 points; the ranking of cost centres, not the second digit, is the finding.** A rerun with cooldown between repetitions (or fixed clocks) is cheap and should precede any before/after comparison.
3. Ablation shares are differences of separately timed passes; the encoder-side passes are not fused like the decoder, so shares do not sum exactly to 100 %.

## Environment

| | |
|---|---|
| OS | Windows 11 Pro 10.0.26200 |
| CPU | 11th Gen Intel Core i7-1185G7 (4C/8T, nominal 3.0 GHz; Windows reported 71 % processor performance and 15–25 % background load during setup) |
| Rust | rustc/cargo 1.99.1, host `x86_64-pc-windows-gnu`; release profile identical to production (opt 3, thin LTO, 1 CGU, no `target-cpu`) |
| WPR | present, `wpr.exe` 10.0.26100.9818 |
| WPA / xperf / VTune / cargo-flamegraph / samply / addr2line / objdump | **not installed** |
| Elevation | the shell is **not** administrator; WPR kernel sampling needs elevation |

**Profiler status and proposal (nothing installed, as instructed).** WPR can record but cannot be analysed without WPA, and recording needs an elevated shell. The single official path is: install the *Windows Performance Toolkit* component of the Windows ADK (Microsoft, installs `wpa.exe`, `xperf.exe`), then record from an elevated terminal with `wpr -start CPU -filemode`, run one decode, `wpr -stop decode.etl`, and open in WPA. PMU counters (cache misses, branch mispredicts) additionally need `wpr -start CPU` with PMC sources via `xperf -pmcprofile`; on this laptop the hypervisor may hide them. **Approval needed before I install anything.** Because the hot code is almost entirely inlined (`code_bit`, `scan_tree` are `#[inline(always)]`), a function-level sample profile would mostly show `decode_planes` as one block; the ablations below give finer attribution than function-level sampling would.

## Selected workloads (same compressed files for every run)

| image | size | bits | samples | ULISA file | payload bpp | archived decode (best of 2) |
|---|---|---|---|---|---|---|
| `pentax_k1ii_station` (heavy) | 7392×4950 | 14 | 36,590,400 | 34,932,961 B | 7.64 | 11.99 s (3.05 Mpx/s); JXL e7 7.02 s, e9 5.46 s |
| `leica_m10_brick` (slowest per sample, highest bpp) | 5984×3992 | 14 | 23,888,128 | 26,298,847 B | 8.81 | 10.99 s (2.17 Mpx/s) |
| `sigma_fp_bike8` (small, 8-bit, smooth) | 3856×2170 | 8 | 8,367,520 | 2,661,037 B | 2.54 | 1.10 s (7.60 Mpx/s) |

Decoded sample counts equal the sample counts above (one residual per Bayer sample). `lattice=None` for all three.

## Commands

```
# build (own target dir; production target/ untouched)
cd benchmarks/ulisa-decode-profile/harness
CARGO_TARGET_DIR=<scratchpad>/tgt cargo build --release --offline
# every run: High priority, pinned to logical CPU 4, single process
phases  <file.ulisa7.pkon> 3        # public-API phase timing of pkon::decode_ulisa, CRC-verified
ablate  <file.ulisa7.pkon> 3-6      # P / M / E / D ablations, allocation counter, decision statistics
model   <file.ulisa7.pkon> 3        # model-only replay, flank on/off, flank-base cost
predict <file.ulisa7.pkon> 3        # predictor arithmetic micro-ablation (divisions)
```
Raw outputs: `raw/phases_*.txt`, `raw/phases_pinned_*.txt`, `raw/ablate_*.txt`, `raw/model_*.txt`, `raw/predict_*.txt`.
Integrity check built into `ablate`: re-encoding the decoded image with the production encoder path reproduces the stored payload **byte for byte** on all three files, so the replay statistics describe the real stream.

## Baseline

CPU utilisation: the decoder is single-threaded; process CPU time ≈ wall time (k1ii ablation run 203.8 s CPU / 204.6 s wall, leica 106.1 / 106.3), i.e. ~12.5 % of the 8 logical CPUs.

| image | `pkon::decode` (pinned runs) | `decode_planes` isolated (best) | ns/sample |
|---|---|---|---|
| k1ii | 11.2 s first rep, 16–19 s later reps | 19.3 s (throttled) | 529 |
| leica_m10_brick | 10.7, 10.8, 13.7 s | 8.06 s | 337 |
| sigma_fp_bike8 | 1.0, 1.7, 1.9 s | 1.05 s | 125 |

Profiling overhead: no instrumentation sits inside the codec. The harness calls the unmodified public API; the counting allocator adds ~nothing at 89 allocations per decode. Ablations replace only the `BitIo` back-end. So the baseline and "profiled" timings are the same code.

## Where the time goes

### Phases of `pkon::decode_ulisa` (public-API replica, CRC verified)
`BayerFrame` setup, topology, `FlankCodec::new`, buffer allocation: < 0.001 s. **`decode_planes`: 97–99 %.** `merge`: 0.03–0.11 s. `denormalize`: 0 (no lattice). `pixel_crc_u16`: 0.12–0.39 s (≈ 240 MB/s bytewise CRC). Post-processing total ≈ **1.5–2.5 %**.

### Ablation of `decode_planes`
P = Stage 1 predictor + structural leaf only. M = whole model, no range coder (`NullIo`). E = real encoder. D = real decoder.

| | sigma_fp_bike8 (8-bit) | leica_m10_brick | k1ii (throttled) |
|---|---|---|---|
| D decode_planes | 1.045 s | 8.061 s | 19.34 s |
| P predictor + leaf | 37 % | 11.8 % | 12.3 % |
| M − P model + flank bases + context mixing | 60 % | 55.5 % | 40.3 % |
| D − M range decode + decode-side dependency | 3 % | 32.7 % | 47.4 % |
| D / E (decoder vs encoder, same coder) | 0.93 | 1.11 | 1.36 |

The range coder is expensive on 14-bit data: encode-side `E − M` is 0.11 s / 1.86 s / 4.10 s. On the heavy images the decoder is 10–36 % slower than the encoder running the identical model and coder, which is the signature of the serial decode dependency (the next decision's contexts need this decision's bit; the encoder knows all bits up front and overlaps freely).

### Decision statistics (from the verified replay)
| | bike8 | leica | k1ii |
|---|---|---|---|
| binary decisions / sample | 3.06 | 14.04 | 11.52 |
| of which raw p = ½ mantissa bits | 0.01 | 3.69 (26 %) | 2.38 (21 %) |
| ns / decision (decode) | 41 | 24 | 46 (throttled) |
| expected data-dependent mispredicts / sample (Σ min(p,1−p)) | 0.94 | 3.66 | 3.09 |
| decisions with min(p,1−p) ≥ 40 % (essentially coin flips) | 27 % | 42 % | 41 % |

About a quarter of all decisions are unpredictable for a branch predictor if the bit resolution compiles to a branch. Whether `Stage4Dec::code` compiles to a branch or a conditional move was **not verified** (no disassembler installed; checking is step 1 of the recommended experiment).

### A. Entropy decoding
`Stage4Dec::code`: one 32×16 multiply, compare, subtract, renormalise loop with a bounds-checked `buf.get(pos)` per refill. Per-decision work around it in `code_bit`: 4 context-slot loads (3 table + 1 flank), 4 × `s3_input` (STRETCH lookup × CONF), 5-term i64 dot product, SQUASH lookup, 5-weight update with clamp, then 3–4 `s3_update_slot` (i64 multiply, RECIP lookup, count increment). Roughly 70–140 cycles/decision measured. Raw bits (`io.code(32768, …)`) run through the same full range-decoder path.

### B. Context access
Working set per plane: slot table 55,360 × 4 B ≈ 221 KB, flank slots 64 × 210 × 4 B ≈ 54 KB, weights ≈ 15 KB, STRETCH/SQUASH 16 KB, RECIP 1 KB: **~300 KB, resident in L2 (1.25 MB), exceeding L1D (48 KB).** Slots for consecutive nodes of one context sit in the same 256-byte group, so intra-sample locality is good; the first touch of each context per sample is an L1 miss / L2 hit. Not measurable here (no PMU). The 12 structural leaves are heavily skewed (leaves 8–10 hold ~85 % of 14-bit samples), so the hot set is smaller than 300 KB in practice. The per-plane `Stage3` initialisation is < 0.1 ms; `FlankModel` carries over.

### C. Reconstruction
Predictor (`predict`, `Shrub`): about 36–44 ns/sample in isolation (≈ 12 % of decode on 14-bit, 37 % on the smooth 8-bit file). It contains one **u128 division** and three u64 divisions per sample. Replacing them with shifts in a copy (wrong results, timing bound only) saves only 4–12 ns/sample (plane 0: 44.1→32.2, 40.3→34.9, 36.5→29.9 ns), i.e. **≈ 1–3 % of 14-bit decode time** and not on the critical chain. Merge/denormalise/CRC: see phases. Flank-base construction: ≤ 0.4 s upper bound (harness copy; production uses slice fast paths), ≈ 2–5 %, on the critical path between planes.
Allocation: 89 allocations per decode; 127 / 360 / 551 MB requested (≈ 15 B per sample: i32 residuals, i16 stored residuals, i32 flank sums, u8 bases, output planes, merged output); peak live +48 / 138 / 211 MB. Zero-fill and page-fault cost is bounded at ≲ 1 % (not separately measurable).

### D. Cross-plane dependencies (why planes are not parallel)
* Planes are coded 0,1,2,3 into **one** range-coder stream with no per-plane length or restart. Plane k+1's bits start where plane k's end, so plane k+1 cannot begin until plane k is fully decoded.
* The `FlankModel` (probability slots and sign-stage weights) is **shared and adaptive across planes**: plane k+1 starts from the state plane k left behind.
* Flank links: plane k reads only residuals of lower-numbered planes at mosaic neighbours (`k^1` horizontal, `k^2` vertical, clamped at edges). Bases for plane 1 and 2 depend only on plane 0; plane 3 on planes 1 and 2.
* Independent per plane: the *computation of bases* (bit-exact, only needs finished earlier planes, so plane 2's bases could be built while plane 1 decodes), merge/CRC (after all planes), and nothing else. Within a plane, every sample's leaf and contexts depend on the previous sample's reconstructed value and magnitude, so the plane is a strict serial chain.
* Therefore: any thread-level speed-up of the entropy loop needs independent streams/model state, i.e. a **bitstream change**. Out of scope here, but it is the only route to a multiple.

## Ranking of optimisation opportunities

Gain estimates are judgement from the above, not measured; compat/ratio columns are certain by construction unless stated.

| # | Change | Expected decode gain | Complexity | Bitstream risk | Ratio risk |
|---|---|---|---|---|---|
| 1 | **Branch-free `Stage4Dec::code`** (mask arithmetic for bit/range/code update; branch-free refill with unchecked/padded buffer) and a dedicated raw-bit path for p = ½ | 5–15 % on 14-bit (est.) | low | none (identical arithmetic) | none |
| 2 | Compile with `x86-64-v3` (lzcnt/BMI/cmov-friendly codegen; ~10 `leading_zeros` per sample become one instruction) or runtime-dispatch it; no source change | 0–10 % | trivial (policy: reproducible portable build) | none (integer arithmetic) | none |
| 3 | Restructure `code_bit`: precompute both update outcomes (bit 0/1) of the slot/weight updates before the bit is known so the dependent tail is a select; SIMD weight update/dot (5 lanes); drop redundant clamps | 10–25 % (est.) | medium | none if verified against replay | none |
| 4 | Post-processing: slice-by-8/PCLMUL CRC (same CRC value), fuse merge+CRC, build plane-2 bases during plane 1 | 2–4 % | low | none | none |
| 5 | Predictor divisions → exact u64 fast path where the u128 operands fit (must prove exactness for all inputs, or test exhaustively on edge ranges) | 1–3 % | medium | none if exact | none |
| 6 | (excluded) Independent streams per plane or stripe for threads; fewer decisions per sample (e.g. direct multi-bit mantissa coding) | up to ~4× threaded; decisions change | high | **breaks format** | small loss from model restarts / precision |

None of 1–5 will close a 1.9× gap by itself; together they plausibly reach 1.2–1.5×.

## Recommended first experiment

**Make the range decoder's bit resolution branch-free and give the raw p = ½ bits a lean path (item 1).**
Why first: it is the one cost that the numbers isolate on the dominant workload (decode-side `D − M` is 33–47 % on 14-bit files, and decoding is slower than encoding by 10–36 % with the same model), a quarter of decisions are coin flips, and it touches ~30 lines with exactly reproducible arithmetic. Protocol: (a) emit assembly for the harness to see whether `code()` is currently a branch; (b) implement in a copy of `Stage4Dec` in this harness crate, not in `src/`; (c) require byte-identical output on all 23 common images via the CRC checks; (d) compare `decode_planes` on leica_m10_brick and k1ii with cooldown between reps and first-rep-only numbers; (e) keep only if ≥ 5 % and stable. If it shows < 3 %, the chain is bound by the model update latency, and item 3 becomes first. If the evidence of (a) shows `code()` is already branch-free, skip straight to item 3.
