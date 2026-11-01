# ELLIE Pareto experiment (Game 3): compression vs compute

Production `src/ellie.rs`, the PKON format and `Cargo.toml` dependencies are untouched. The only repo edits are two `[[bin]]` entries in `Cargo.toml` for the experiment binaries. Nothing is committed.

## 1. What the implementation actually exposes

| Item | Finding |
|---|---|
| Stage 3 "effort" parameters | None are configurable. Everything is a hardcoded private constant. The real knobs, all used by my scratch copy: `S3_LIMIT` (127, count cap, i.e. adaptation-rate floor), `S3_MIX_SHIFT` (10, mixer learning rate), which of the four context slots A/B/C/D are mixed (production: all four), and how many mantissa bits are modelled (production: 2, rest raw). Context quantiser sizes (`NQ`/`ND`/`NW`), `CONF`, and weight-set selection were not varied. |
| Stage 4 | The range coder has no effort parameter. Only its termination (per plane) varies. It was left alone. |
| Model reset | Stage 3 resets at the start of every plane (`carry_model = false` in production). Stage 2 restarts per plane. The range coder is terminated and flushed per plane, so each plane is its own stream. Reset boundary = termination boundary = one plane. |
| Planes | The container encodes Y (8-bit), Co and Cg (9-bit) as three independent streams. Per-plane configuration is therefore legitimate: the three streams share no state. Co and Cg reuse one `EllieCodec`, reset between them. |
| Buffers | One `res: Vec<i32>` (4 MiB for a 1024² plane) per `EllieCodec`, written whole by Stage 1, bias-corrected in place by Stage 2, then read whole by Stage 3. The model state is about 128 KiB. |

## 2. Method

- **Scratch implementation:** `exp_ellie_pareto.rs` contains a copy of Stage 3 with `MASK` and `MB` as const generics and limit/shift as runtime values. It is driven by the unchanged production Stage 1, Stage 2, Stage 4 and YCoCg-R, and replicates the container's plane-splitting logic.
- **Correctness gate:** the scratch Stage 3 at production settings is byte-identical to production on every image, and its total of 43,578,526 B matches the published 50-image baseline exactly.
- **Corpus:** the same 1024-px centre crops. The 3-image set is `portrait_0`, `screen_0` (RGBA, alpha plane coded unchanged) and `night_0`. The 50-image set comes from `dataset50.json`.
- **Timing:** release build, single thread, process at high priority and pinned to one core. Timing covers the end-to-end path: plane split, YCoCg-R forward, codec setup and encode for encoding; decode, inverse transform and interleave for decoding. Configs are interleaved with a rotating order per repetition. I report the sum over images of the per-image minimum. The 3-image runs use 7–11 repetitions and the 50-image run uses 3.
- **Machine:** an i7-1185G7 laptop (4C/8T) with Firefox and VS Code open. It is noisy.
- **Noise:** the first Axis A run was not pinned. Its spreads were 30–90% (median over min), and zero-cost parameters such as limit and shift showed fake slowdowns of +20–50%. **Treat that run's timings as invalid.** Its sizes are exact and are used. The pinned runs show a 3–12% median-over-min spread. Differences under about 5% in aggregate time are not distinguishable. Per-image times are far noisier (for example `portrait_0` shows a +36% outlier).
- **Every row** in every CSV round-trips pixel-identically and is deterministic across repetitions. Encoder and decoder use the same per-plane configuration.

## 3. Axis A: entropy-model effort (uniform across planes, 3-image set)

Aggregate over the 3 images. Size deltas are exact. Timing is from the pinned runs, except where noted.

| Change | Size | Enc time | Verdict |
|---|---|---|---|
| Production | 2,733,439 B | 0.88 s | baseline |
| `S3_LIMIT` 30 / 60 / 255 | +0.27% / +0.08% / −0.05% | none (no extra work) | 255 slightly better; lower is worse |
| `S3_MIX_SHIFT` 8 / 9 / 11 / 12 | +1.58% / +0.50% / −0.18% / −0.21% | none | slower mixer learning helps, flattening at 11–12 |
| limit 255 + shift 11 (all contexts) | −0.15% | +1.5% (noise) | free, small |
| Drop context D (order-0 root) | −0.19% | −4.7% | D is nearly useless |
| Drop A (qS) | −0.05% | −3.8% | A is nearly useless |
| Keep only B + C | −0.22% | −11% to −15% | much cheaper, slightly better |
| Keep only A + C | +0.03% | −13% | cheap, no loss |
| Keep only B | +1.07% | −28% | steep loss |
| Keep only C | +1.91% | −28% | steep loss |
| A + D only | +1.98% | −15% | bad |
| D only (order-0) | +7.92% | −8% | very bad |
| Modelled mantissa bits 1 / 0 | +0.29% / +2.01% | −4.5% (mb0) | little time saved for the loss |

**Diminishing returns:** the four-context production model is past the point of paying for itself. Contexts B (energy × disagreement) and C (long-run energy × west magnitude) carry nearly all the information. Adding A and D costs about 13% of encode time for a net compression loss of about 0.2%.

## 4. Axis B: asymmetric plane effort

The hypothesis was that Y deserves more modelling effort and Co/Cg less. **The measurements do not support it.**

- Light chroma on top of a full Y model (3-image set): `Y full / Co,Cg = A+D` costs +0.71% for −6.9% encode time. `Y full / Co,Cg = B+D` costs +0.31% for −6.0%. Both are dominated by simply using B+C for all planes (−0.22%, −15%).
- Reverse asymmetry (light Y, full chroma, `Y A+D / chroma full`): +1.27% and only −3.7% time, which is worse on both axes.
- Chroma planes are not cheap to model: on `night_0` Co and Cg together are larger than Y.
- The asymmetric configuration that does reach the frontier is `Y = B+C, Co,Cg = B only` (`B-YBC-CB`). It gives −0.02% size and −20.9% encode time on the 50 images. That is a legitimate intermediate point between U-BC and U-B, with the lighter model on chroma and the heavier one on Y.
- Hypothetical 7–8 s encode at 3.59×: not reached. The fastest tested point is 10.27 s (U-B), at 3.605×.

## 5. Axis C: buffering and windowing

**Not worth pursuing, and not testable without production changes.**

- Measured (`exp_ellie_axisc.rs`, output in `results/axisC_probe.txt`): the full-plane residual buffer costs 0.11–0.29% of encode time per pass, and `EllieCodec::new` costs 0.03–0.06 ms. Stage 1+2 take 16–22 ms of 50–130 ms per plane. Stage 3+4 are the bottleneck and work on the 128 KiB model, not on the big buffer.
- Fusing Stages 1/2/3 into cache-sized row bands would keep the statistical history unchanged in principle, but Stage 1 `scan` is a private whole-plane function. Making it resumable means modifying Stage 1, which is out of scope. The measured upper bound on the gain is about 0.3%, so I did not build it.
- No model-reset or range-coder-termination window sizes were tested. Resetting at arbitrary boundaries would only lose statistical history and add termination bytes.
- Caveat: `EllieCodec::new` timing does not include first-touch page faults of the zeroed buffers. That cost is also small relative to the plane work.

## 6. Results on the 50-image benchmark

Baseline reproduces the published numbers: 43,578,526 B, ratio 3.633. Timing is the sum of per-image minimums over 3 repetitions.

| Config | Bytes | Ratio | Size vs prod | Enc s | Enc vs prod | Enc MP/s | Dec s | Dec vs prod | OK / det |
|---|---|---|---|---|---|---|---|---|---|
| PROD (production EllieCodec) | 43,578,526 | 3.633 | 0 | 13.51 | 0 | 3.88 | 15.70 | 0 | yes / yes |
| scratch-default (control) | 43,578,526 | 3.633 | 0 | 13.49 | −0.2% | 3.89 | 15.89 | +1.2% | yes / yes |
| U-all-l255s11 (free params only) | 43,503,936 | 3.640 | −0.17% | 13.59 | +0.6% | 3.86 | 16.06 | +2.3% | yes / yes |
| U-noD-l255s11 | 43,440,278 | 3.645 | **−0.32%** | 12.85 | −4.9% | 4.08 | 15.01 | −4.4% | yes / yes |
| B-YnoD-CBC | 43,455,103 | 3.644 | −0.28% | 12.14 | −10.1% | 4.32 | 14.40 | −8.3% | yes / yes |
| U-BC | 43,477,924 | 3.642 | −0.23% | 11.58 | −14.3% | 4.53 | 14.00 | −10.8% | yes / yes |
| **U-BC-l255s11** | 43,451,383 | 3.644 | −0.29% | 11.66 | −13.7% | 4.50 | 14.18 | −9.7% | yes / yes |
| U-BC-mb1-l255s11 | 43,555,347 | 3.635 | −0.05% | 11.31 | −16.3% | 4.63 | 13.76 | −12.4% | yes / yes |
| B-YBC-CB | 43,568,809 | 3.634 | −0.02% | 10.69 | −20.9% | 4.90 | 13.27 | −15.5% | yes / yes |
| **U-B** | 43,631,701 | 3.629 | +0.12% | 10.27 | **−24.0%** | 5.10 | 12.64 | −19.5% | yes / yes |

`B-YnoD-CBCmb1` also ran, at 43,498,802 B and 12.05 s, and is dominated.

Config naming: `U-` = same on all planes; `B-` = per-plane. Contexts: `noD` = A+B+C, `BC` = B+C, `B` = B only. `l255s11` = `S3_LIMIT` 255 and `S3_MIX_SHIFT` 11. Per-plane spec syntax `m<mask>b<mb>l<limit>s<shift>`, with 15/2/127/10 = production.

Decode time tracks encode time for every config (the model is symmetric). No config shows a decoding regression that encoding hides, except the near-zero-cost parameter-only config, which sits within noise.

## 7. Pareto frontier (bytes vs encode time, 50 images)

Slowest/smallest to fastest/largest:

1. U-noD-l255s11: 43,440,278 B, 12.85 s
2. U-BC-l255s11: 43,451,383 B, 11.66 s
3. U-BC: 43,477,924 B, 11.58 s (marginally dominated in spirit by the previous point on size; kept because the timing difference is within noise)
4. U-BC-mb1-l255s11: 43,555,347 B, 11.31 s
5. B-YBC-CB: 43,568,809 B, 10.69 s
6. U-B: 43,631,701 B, 10.27 s

Dominated (another config is both smaller and faster): production, scratch-default, U-all-l255s11, B-YnoD-CBC and B-YnoD-CBCmb1. In particular, production is beaten on both axes by every frontier point except U-B, which is faster but 0.12% larger.

## 8. The four answers

1. **Best compression:** `U-noD-l255s11`, contexts A+B+C, limit 255, shift 11 on all planes. −0.32% (−138 KB over 50 images), ratio 3.645, and smaller than production on 50 of 50 images. Also 5% faster. It is the least risky change.
2. **Fastest encoding:** `U-B`, context B only. −24.0% encode time (5.10 MP/s) and −19.5% decode, for +0.12% bytes. It is smaller than production on only 18 of 50 images.
3. **Best balanced:** `U-BC-l255s11`, contexts B+C, limit 255, shift 11 on all planes. 13.7% faster to encode, 9.7% faster to decode and 0.29% smaller. It is smaller than production on 46 of 50 images; the worst image loses +0.11%.
4. **The knee:** the knee is at two contexts (B+C). Going from four contexts to B+C buys both speed and size. Going on to B only buys another ~10 points of speed but costs +0.4% in size relative to B+C. Going the other way, adding back A, D or both costs time and returns at most 0.03% in size (noD vs BC) and nothing for the full set. Production is therefore past the knee on the expensive side. Free-parameter tuning (limit 255, shift 11) is worth about 0.06–0.17% with no compute cost.

Per-image results for the three key candidates are in `results/per_image_best.md`. Per-image times come from best-of-3 and are noisy (single-image swings of ±15 points are normal), so rely on the aggregates.

## 9. Worth integrating?

| Change | Verdict |
|---|---|
| `l255s11` free parameters alone | Marginal (−0.17%). Only worth bundling with a context change. |
| Drop context D | Yes if you want smaller files with a small speed gain. |
| B+C contexts + `l255s11` | **Yes, the strongest candidate.** Faster and smaller, and the worst image loses only 0.11%. |
| B only | Only if speed matters more than 0.1–0.4% of size. |
| Asymmetric planes | **No, with one exception:** `Y = B+C / chroma = B` is on the frontier but offers nothing over choosing a point between U-BC and U-B. The "heavy Y, light chroma" hypothesis failed. |
| Mantissa-bit reduction | No. The time saved is small for a measurable loss. |
| Buffer/window sizing | No. Bounded at about 0.3%, and needs Stage 1 changes. |

Integration caveats:
- **Format compatibility:** every config changes the bitstream. Existing `.pkon` files would not decode with a changed Stage 3, so this needs a format revision or a representation flag in the metadata. I did not touch the container.
- **Selection bias:** the exploration was done on the 3-image set. The 50-image set includes those three images, so it is not an independent held-out check. The size gains are, however, consistent across images (U-BC-l255s11 smaller on 46/50; U-noD-l255s11 smaller on 50/50).
- **Not tested:** quantiser sizes (`NQ`, `ND`, `NW`), `CONF`, mixer weight-set selection, 16-bit planes, untransformed (non-YCoCg-R) images, single-context alternatives other than B and C, and context A/D replaced by cheaper alternatives. The experiment could not exceed the set of knobs already in the code, as instructed.
- **Generic code:** speed numbers come from the const-generic scratch implementation. A hand-specialised production version should be about the same or slightly faster, but I did not verify that.

## 10. Files

- Experiment code: `benchmarks/ellie-pareto-game3/exp_ellie_pareto.rs` (main harness and scratch Stage 3), `benchmarks/ellie-pareto-game3/exp_ellie_axisc.rs` (Axis C probe), `benchmarks/ellie-pareto-game3/analyze.py` (aggregation and frontier).
- Config files: `benchmarks/ellie-pareto-game3/cfgs/` (`axisA.txt`, `phase2.txt`, `phase3.txt`, `final50.txt`).
- Raw results: `benchmarks/ellie-pareto-game3/results/` — `axisA_3img.csv` (timings unpinned and invalid, sizes valid), `phase2_3img.csv`, `phase3_3img.csv`, `final_50img.csv`, each with a `_summary.md`; `per_image_best.md`; `axisC_probe.txt`.
- Registered in `Cargo.toml` as `exp_ellie_pareto` and `exp_ellie_axisc`. Run with `cargo run --release --bin exp_ellie_pareto -- --set 3|50 --reps N --cfgs <file> --out <csv>`.
