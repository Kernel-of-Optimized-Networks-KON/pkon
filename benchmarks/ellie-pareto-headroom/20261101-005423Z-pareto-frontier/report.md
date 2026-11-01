# ELLIE: Pareto frontier and remaining compression headroom at 3.17 MP/s

Analytical only. No benchmark, timing run, sweep or code change. Everything below is read from frozen artifacts or derived from them by `derive.py` (same folder; it reads the artifacts and writes `results.json` and `pareto_frontier.png`).

Sources: `benchmarks/ellie-full-bench/20261101-000449Z-ellie-vs-bench50/` (new run), `benchmarks/ellie-stage4-3img/.../results.json`, `src/ellie.rs`, and the historical `benchmarks/pkon2/20261027-155433Z-bench50/` (same 50 crops, byte-identical per the new report).

## Answer

**Proven headroom at 3.17 MP/s: about 0.** Stage 4 wastes 16 B of 69,186,801 B (0.00002%). The only other zero-compute items I can bound are a probability-clamp cap of at most 0.14% (loose, unattainable) and unmodelled raw mantissa bits of at most 0.07% (under a geometric-residual assumption). Together they are at most roughly 0.2%, about 100 kB, and the real figure is much smaller.

**Reference gap, not proof:** a retired ELLIE-generation codec (PKON2 X4PJ) stored the same 50 images in 38,913,741 B payload, i.e. 4.069×, at 3.08 MP/s encode in an earlier session. That is 30,273,060 B (43.8%) smaller than current ELLIE, and 1.78× in ratio. Its speed is 2.9% under the budget as measured. Normalised for machine drift between the two sessions (the same competitors ran 3.7–12.6% faster in the new session) it would be 3.19–3.47 MP/s. That normalisation is an assumption, not a measurement. Its source is not in the tree, so I cannot attribute its gain to any mechanism. It shows that this corpus and this per-sample time budget (about 104.5 ns/sample) can hold a model roughly 1.8× better. It does not show that the current architecture can get there without redesign.

**Where the opportunity is:** not Stage 4. The whole remaining cost is the Stage 1–3 residual model, and the artifacts cannot split it between Stage 1/2 and Stage 3.

## 1. Baseline and speed budget

| quantity | value | source |
|---|---|---|
| raw / encoded | 158,334,976 B / 69,186,801 B | bench50 |
| ratio, bits/sample | 2.2885×, 3.496 | derived |
| encode / decode | 3.17 / 2.59 MP/s | bench50 |
| budget | 3.17 MP/s = 104.5 ns/sample (315.6 ns/px); no extra compute assumed | derived |
| Stages 1–3 (null-coder run) | 86.3 ns/sample (82.4%) | model-only 13,670 ms |
| Stage 4 range coder | 18.4 ns/sample (17.6%) | full 16,590 − 13,670 ms |

The split comes from two separate best-of-2 measurements, so it is approximate. The Stage 1/2/3 split inside the 86.3 ns is **UNKNOWN**: `src/ellie.rs` tests print it, but no artifact stores it.

Per binary decision, from the 3-image run only (a different, faster session; bench50 stores no decision counts): model 17.0–21.9 ns, coder 3.1–4.3 ns, 2.75–6.97 decisions/sample. Decisions/sample for bench50 is **UNKNOWN**.

## 2. Where the information cost sits

1. **Measured:** the Stage 3 ideal −log2(p) cost is 69,186,785 B and the actual output is 69,186,801 B. All the bytes are set by the probabilities Stage 3 emits. A near-zero Stage 4 overhead says the coder is exact. It says nothing about how close the model is to the limit.
2. **Lower bound on what Stage 3 leaves on the table.** No true entropy of the images is derivable from ELLIE's own data. The diagnostic below only shows the model is weak:
   - For integer X with E|X| = m, H(X) is at most the entropy of a two-sided geometric distribution with that mean (max-entropy property). So a static MED + order-0 coder costs at most H_geo(MAE) per sample.
   - The bench50 metadata gives the G-plane MED MAE per image. Assuming R and B have the same MAE (an assumption), the ceiling is 77.26 MB.
   - ELLIE is only **10.5% below** this ceiling and above it on 2 of 50 images. JXL e9 and the retired codec are about **49.4–49.6% below** it.
   - So the context-mixing stack currently buys little beyond an order-0 model of a simple predictor's residual. Combined with item 1, this places the cost in the modelling of Stages 1–3.
3. **Where the gap sits across images** (ELLIE vs retired codec, split by G MED MAE tercile):

   | tercile (MAE) | ELLIE / retired | share of the 30.27 MB gap |
   |---|---|---|
   | low, 0.70–2.14 | 1.86× | 28.8% |
   | mid, 2.15–3.20 | 2.07× | 38.8% |
   | high, 3.23–13.8 | 1.55× | 32.4% |

   The gap is broad. It is not confined to smooth or to noisy images.
4. **Structural facts in `src/ellie.rs` that bound what Stage 3 can know.** Their value is **UNKNOWN**; no ablation exists.
   - Planes are coded independently, with no inter-plane context.
   - Stage 3 contexts use only neighbouring residual magnitudes of the same plane (qS, qS×qd, qE×qW, order-0) and a sign context of (W, N).
   - Stage 2 is one scalar bias per plane (rate 1/64), not context-conditional.
   - Stage 1 is a fixed 3-predictor blend.
   - There are 4 mixer inputs plus a bias, a single mixer layer, and no secondary estimation.
   - `carry_model` (continuing Stage 3 state across planes) is +0.016% worse, so statistics sharing is not the missing piece.

## 3. Bounded and estimated opportunities

| opportunity | gain | added compute | status |
|---|---|---|---|
| Stage 4 overhead | ≤ 16 B | none | **proven** |
| Probability clamp [32, 65503] / 12-bit squash | ≤ 0.14% (cap: every decision certain, at the max 6.97 decisions/sample; unattainable) | **UNKNOWN** (a 16-bit stretch table is 128 KB instead of 8 KB, a memory fact only) | loose bound |
| Model the k−3 raw mantissa bits | ≤ 0.004 bits/sample ≈ 0.07%, two-sided geometric model, θ swept analytically | at least one extra modelled decision (17–22 ns) per sample with k ≥ 4, up to +16–21% of the budget if applied to every sample; k ≥ 4 fraction **UNKNOWN** | bounded; poor value per ns |
| One more mixer input on every decision | **UNKNOWN** | 9.0–29.2% of the budget (assumes cost splits evenly over 5 mixer terms; Stage 1/2 time is inside the figure, so likely an overestimate) | cost estimated, gain unknown |
| Richer contexts, cross-plane context, better Stage 1/2 | **UNKNOWN**; the reference gap (30.3 MB) shows headroom exists | **UNKNOWN** | unquantified |

Any added compute breaks 3.17 MP/s unless it is paid for by removing compute elsewhere. The artifacts hold no per-component cost or value data, so no budget-neutral reallocation can be proven.

## 4. Pareto frontier

`pareto_frontier.png`, left panel: encode MP/s on a log axis against aggregate ratio, bench50. Measured points are ELLIE now (3.17, 2.289), JXL e7 (1.13, 4.007), JXL e9 (0.28, 4.048), WebP m6 (0.12, 3.543), PNG (1.97, 2.418) and the retired codec (3.08, 4.069). The measured non-dominated set is the retired codec and current ELLIE (staircase). The shaded region right of the 3.17 line is split into two parts:
- A teal sliver, up to about 0.14%, which is the only zero-compute gain with a bound.
- A blue region up to 4.069×, which is reference-only and not proven recoverable.

The exact frontier at the budget cannot be established from the existing evidence, and I plotted no estimated points.

The right panel plots added compute against bounded gain for each opportunity. Unknowns are marked "?" and not drawn as bars.

## 5. Headroom summary

| | value |
|---|---|
| current | 2.289× (69,186,801 B) |
| proven at budget | ≈ 2.289× (≤ 16 B; ≤ ~0.2% by loose bounds, ≈ 2.293×) |
| reference at ≈ budget (different codec, older session) | 4.069× (38,913,741 B) |
| reference gap | 30,273,060 B = 43.8% of ELLIE size; +1.78× in ratio (+77.8%) |
| vs JXL e9 (0.28 MP/s, 11× slower) | 30,067,858 B smaller than ELLIE (JXL e9 = 39,118,943 B) |

## Caveats

- The retired codec was timed in an earlier session and its speed (3.08 MP/s) is under the budget as measured. The 3.19–3.47 MP/s range depends on the assumption that machine drift is common-mode across codecs.
- The legacy file size including its 24-B headers is 38,914,941 B (4.0688×). I used the payload, to match ELLIE's headerless payload.
- The order-0 diagnostic assumes R and B MAE equal G MAE. Only G is in the metadata.
- Decisions/sample and per-decision ns come from 3 images in a different session. bench50 values are unknown.
