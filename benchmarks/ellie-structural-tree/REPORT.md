# ELLIE structural-tree experiment: report

Canary: portrait_0, screen_0, night_0 (1024-px centre crops, YCoCg-R as chosen by production Auto for all three). Release build. Baseline = unchanged `pkon::encode_standard`: **2,612,940 B** (481,178 / 222,580 / 1,909,182). Design: `DESIGN.md`. Code: `exp_ellie_structural_tree.rs`. Data: `results/` (`per_image.csv`, `summary.md`), `run.log`. Production sources untouched; the only tracked file edited is `Cargo.toml` (one `[[bin]]` entry).

## 1. Signals
Selected (causal, integer): **value diversity** of {W,N,NW,NE} (1 / 2 / >=3 distinct), **predictability** (pixel gradient energy vs neighbouring residual energy), **directional coherence** (min vs median of S_H,S_V,S_D), **stationarity** (fast vs slow residual energy by >=2 octaves), plus a smooth-gradient test. Not implemented: multiscale and grid/repetition (no cheap causal estimator that is not a duplicate of existing energy contexts or of the earlier history-search Deja Vu line).

## 2. What the tree controlled (measured separately)
`T` = leaf x qS as an extra mixed probability context (uses the input slot idle under Bc255S11). `M` = mixer weight-set chosen by leaf. `W` = one Shrub state per leaf. 12 fixed leaves, no stored thresholds; 1 mode byte counted in every tree-on size.

## 3-4. Bytes (complete file = production container overhead + streams + 1 byte)
| config | total B | vs prod | portrait | screen | night |
|---|---|---|---|---|---|
| ctrl (tree off, = production bytes, asserted) | 2,612,940 | 0 | 0 | 0 | 0 |
| overhead (leaf computed, unused) | 2,612,940 | 0 | 0 | 0 | 0 |
| dupT (constant-leaf extra qS context: capacity control) | 2,612,289 | -0.025% | -0.21% | -0.12% | +0.03% |
| **T** probability only | 2,585,717 | -1.042% | -2.59% | -5.34% | -0.15% |
| M mixer select only | 2,587,863 | -0.960% | -2.36% | -4.56% | -0.19% |
| **W** Shrub per leaf only | 2,614,697 | **+0.067%** | -0.34% | -0.23% | +0.21% |
| TM | 2,584,902 | -1.073% | -2.61% | -5.41% | -0.18% |
| TW | 2,585,497 | -1.050% | -3.30% | -5.61% | +0.05% |
| TMW | 2,585,284 | -1.058% | -3.20% | -5.68% | +0.02% |
| T without flat/diversity tests | 2,597,790 | -0.580% | -1.57% | -2.24% | -0.14% |
| T without predictability | 2,588,148 | -0.949% | -2.57% | -5.20% | -0.04% |
| T without direction | 2,586,231 | -1.022% | -2.58% | -5.21% | -0.14% |
| T without stationarity | 2,583,317 | -1.134% | -2.60% | -5.56% | -0.25% |
| T diversity only (4 leaves) | 2,589,255 | -0.906% | -2.51% | -5.13% | -0.01% |
| **TM without stationarity (best)** | **2,582,588** | **-1.162%** | -2.62% | -5.65% | -0.27% |

Aggregate -1.16% best; the gain is concentrated in screen_0 (-5.7%) and portrait_0 (-2.6%); night_0 is essentially unchanged (-0.27%).

## 5. Speed (best of 5, in-process; noisy shared machine, +/-15%)
Production single-representation encode/decode sums: enc 1,438 ms, dec 1,589 ms. The harness copy (safe indexing) is slower than production, so compare only within the harness: ctrl enc 1,780 / dec 1,747 ms (sum of 3). Leaf computation alone (`overhead`): 1,800 / 1,744 ms, i.e. within noise. T / TM-nostat: roughly 2,000-2,300 / 2,100-2,400 ms, about +15-30% for the extra context lookup and update per binary decision. Shrub-per-leaf costs nothing measurable.

## 6. Metadata
Fixed tree, adaptive thresholds computed causally: 0 bytes of tree structure. One mode byte counted (3 bytes over the canary, 0.0001%).

## 7. Correctness
Tree-off streams are byte-identical to production (asserted per image). All 16 configs x 3 images: pixel-exact round trip through the fused decoder, and two independent encodes give identical bytes. The decoder interleaves leaf -> prediction -> residual decode -> reconstruction, so the leaf only ever sees past samples.

## 8. Did the tree add information?
**Partly, and not where the brief expected.**
- The gain is real but comes from one distinction: **pixel-domain value diversity** (are W,N,NW,NE all equal / two-level / continuous?) combined with the residual energy ELLIE already tracks. Dropping it halves the gain (-1.04% -> -0.58%); keeping only it recovers most (-0.95%). It is information Stage 3 cannot see because it only sees residual magnitudes, and the dupT control (-0.03%) shows it is not extra capacity. It matters on graphics-like and JPEG-flat content (screen, portrait), not on noisy night.
- Predictability and direction added roughly 0.05-0.1% each: marginal, mostly duplicating residual energy and Stage 1's own weighting.
- **Stationarity hurt** (removing it improves every image). It fires on 18% of night pixels and dilutes statistics already well handled by qS (fast) and E (slow).
- **Controlling predictor weighting failed**: per-leaf Shrub is +0.07% alone and mixed in combination (helps portrait -0.7% on top of T, hurts night). Shrub's global adaptation is already good enough; splitting it just slows learning.
- Mixer-select by leaf matches most of the context gain (-0.96% vs -1.04%) and is nearly free in bits, but overlaps with T (TM only adds 0.03%).

Honest limits: three images; the best config was chosen with knowledge of these results (stat removal), so -1.16% is mildly optimistic until checked off the canary (not done, per instructions). This is a modest gain, not a "tree understands the image" breakthrough; the useful piece is a simple pixel-value-diversity context. Screen_0 remains far from JPEG XL (~112 KB), so the large screen-content gap is not explained by this signal.

## Recommendation
Do not adopt the tree as designed. If anything is carried forward, test the single diversity-by-qS context (T-div4 / flat-only, -0.91 to -0.95%) on a held-out set, with the +15-30% time cost weighed against it. Drop stationarity and per-leaf Shrub.
