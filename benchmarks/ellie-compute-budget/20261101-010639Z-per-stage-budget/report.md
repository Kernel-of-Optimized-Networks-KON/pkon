# ELLIE per-stage compute budget at 3.17 MP/s

Analytical only: no benchmark or experiment was run, and no code or historical artifact was touched. `derive.py` (same folder) reads the frozen artifacts and writes `results.json` and `compute_budget.csv`.

Sources: `benchmarks/ellie-full-bench/20261101-000449Z-ellie-vs-bench50/` (`results.json`, per-image `enc_ms` and `model_only_enc_ms`), `benchmarks/ellie-stage4-3img/.../results.json` (per-decision counts and timings for 3 images), `src/ellie.rs`.

## Summary

1. **Additional compute ELLIE can absorb at 3.17 MP/s: 0 ns/sample.** The budget (104.5 ns/sample) is the current measured cost. The exact figure is 3.1685 MP/s (52,428,800 px / 16.547 s), so the rounded 3.17 target is already about 0.05% (0.05 ns/sample) over what was measured. Session-to-session variation on the same images is −6.3% and +9.5% (portrait_0, night_0: 3-image experiment vs bench50), so differences under about 5 ns/sample cannot be resolved by existing measurements. That is noise, not headroom.
2. **Reclaimable compute: not measurable from existing artifacts.** Only upper bounds exist (below). Nothing shows that any stage can be made cheaper without losing compression.
3. **Most plausible capacity:** Stage 3. It is the only stage large enough to fund a meaningful trade: at least 57 ns/sample, at least 55% of the budget. Stage 4 is the only cleanly measured stage, 18.4 ns (17.6%). Stages 1 and 2 are bounded together at no more than about 29 ns (28%) and cannot be separated.
4. **Measured vs uncertain:** the total and Stage 1–3 together are measured. Stage 4 is measured by difference. The Stage 1+2 / Stage 3 split is a bound derived from two regressions. Stage 1 vs Stage 2 is not separable from any stored artifact.

## Compute budget table (ns/sample; budget 104.5)

| Stage | Current cost (ns/sample) | Current share | Unallocated headroom | Reallocatable headroom | Max stage cost, others unchanged | Evidence / confidence |
|---|---:|---:|---:|---:|---:|---|
| 1 Prediction | not isolated | not isolated | 0 | unknown | = current | No stored timing. Jointly with Stage 2 ≤ 29. **None** |
| 2 Bias | not isolated | not isolated | 0 | unknown | = current | No stored timing. About 6 integer ops/sample by code reading. **None** |
| 1+2 combined | ≤ 23.8–29.3 | ≤ 23–28% | 0 | unknown | = current | Intercept of per-sample cost vs content, two fits. **Low–medium (upper bound)** |
| 3 Probability model | ≥ 57.0–62.5 (≤ 86.3) | ≥ 55–60% | 0 | unknown | = current | Stages 1–3 (86.3, measured) minus the Stage 1+2 bound. **Low–medium (lower bound)** |
| 4 Range coder | 18.4 | 17.6% | 0 | unknown | = current | Full encode minus null-coder encode over 50 images. 3-image cross-check: 3.1–4.3 ns/decision × 4.6 decisions/sample = 14–20 ns. **Medium–high** |
| Total | 104.5 | 100% | 0 | – | 104.5 | Measured. Session variation −6% to +10%. **High** |

"Unallocated headroom" is 0 for every stage because the throughput target equals the measured throughput. "Reallocatable headroom" is "unknown" rather than a number because no artifact shows how much of any stage is removable at equal compression. The most that could be removed from a stage is its own current cost, which is not a realistic figure. The same budget cannot be assigned to several stages independently. Each ns/sample added anywhere must be paid for by a ns/sample removed elsewhere.

## How the split was derived

- **Measured:** the total is 16,547 ms net over 158,334,976 samples, or 104.5 ns/sample. Stages 1–3 with a null coder are 13,670 ms, or 86.3 ns/sample. Stage 4 is the difference, 18.4 ns/sample. The two are separate best-of-2 runs. The null-coder run includes plane split, Stage 1, Stage 2 and Stage 3, and Stage 4 is the marginal cost of the coder in context, which is the quantity that matters for budgeting.
- **Per-sample vs per-decision:** `Stage3::code_sample` does its work per binary decision (about 60 integer ops each: 4 slot loads, 4 stretch loads, a 5-term dot product, squash, 5 weight updates, 4 slot updates) plus a per-sample part (four quantisers, neighbour reads, row swaps). Stage 1 and Stage 2 are per-sample only. So cost is `F + b × decisions/sample`, and the intercept F bounds Stage 1 + Stage 2 + Stage 3's per-sample part from above.
  - 3-image fit (3 points, 1 degree of freedom, residuals ±1.2 ns): F = 23.8 ns, b = 13.6 ns/decision.
  - 50-image fit against bits/sample (decision counts are not stored for bench50): F = 29.3 ± 7.3 ns, R² = 0.58. This is noisy and gives no more than a consistency check.
  - Both assume the cost is linear in content. Cache effects or branch behaviour that vary with content would bias F.
- **Implied mean decisions/sample on bench50:** about 4.6 (derived, low confidence). This is consistent with Stage 4's measured 18.4 ns, which comes out at 14–20 ns from the per-decision rate.

## What an addition costs

Costs differ by stage:

- **Stages 3 and 4:** cost scales with decisions/sample, which ranges from 2.75 to 6.97 across the three images. The aggregate is time-weighted, so noisy images dominate: night_0 costs 148 ns/sample against 69 for screen_0. Each +1 ns per decision costs about 4.6 ns/sample on average and up to 7 on noisy images.
- **Stages 1 and 2:** cost is per sample and the same on every image.

Using the 3-image rate (13.6 ns per modelled decision, 3.1–4.3 ns per coder decision):

| Budget | ns/sample | Equivalent in Stage 3 |
|---|---:|---|
| 1% | 1.05 | 0.08 decisions/sample |
| 5% (noise floor) | 5.2 | 0.38 decisions/sample |
| 10% | 10.5 | 0.77 decisions/sample, about +17% of the mean decision count |

## Not measurable from existing artifacts

- **Stage 1 vs Stage 2 vs Stage 3's per-sample part.** `src/ellie.rs` has unit tests that print ns/px for Stage 1, ns/sample for Stage 2 and ns/sample for Stage 3 (around lines 1286, 1417 and 1735), but their output is not stored anywhere. They also run on synthetic data, not the bench50 mix.
- **Stage 4 alternatives.** The raw-bit path (p = 1/2, no multiply needed) and any cheaper coder have no stored timing, so Stage 4's reclaimable amount is unknown.
- **Absolute clock and CPU.** `envinfo` output is not stored in these artifacts, so op counts could not be converted to ns. The Stage 1 division (one 64-bit divide per pixel) is the single costliest known operation, but its latency is unknown.
- **Run-to-run variance for ELLIE.** Only two sessions exist for the same images, so there is no variance estimate.

## Missing measurement

A one-off split timing over the bench50 images (or the 3-image set), same build and harness, with three variants of the existing encode: Stage 1 only, Stage 1 + Stage 2, and Stages 1–3 with a null coder. Together with the existing full-encode timing this would give exact per-stage ns/sample, replacing the bounds above, and would also store decisions/sample for bench50. It would add 3 passes over the corpus (about 15–30 s of encode time in total at current speed) and would need a new harness, since the existing artifacts cannot provide it. I have not run it. Say if you want it.

## Caveats

- The Stage 1+2 / Stage 3 split is a bound from linear fits. If the true cost is non-linear in decisions, the bounds move.
- Stage 4 by difference comes from two separate runs, so per-image values are noisy (the 50-image per-image fit has R² 0.12). Only the aggregate is used.
- The 3-image runs were taken in a different session (−6% / +10% against bench50), so their per-decision rates carry about ±10%.
