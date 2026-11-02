# Texture identity + spatial scale for the structural tree

Source: `exp_ellie_texture_scale.rs` (copy of `../simplify/exp_ellie_tree_simplify.rs`; production, predictor, Shrub, probability model, entropy coder untouched; only a `[[bin]]` entry added to Cargo.toml). Logs: `screen_a.log` (single signals), `screen_b.log` (combinations), `final.log` (5 reps, `results_final/per_image.csv`). Canary: portrait_0 / screen_0 / night_0, same 1024 crop, release, complete-file bytes (+1 mode byte, included). Not committed.

## Foundation (reproduced)
- production 2,612,940 (byte-identical payload asserted); REF-BEST (B,C,T, no confidence, leaf mixer) **2,582,116** reproduced exactly.
- Note: the brief lists "one mixer weight set" *and* 2,582,116, but those are two different earlier configs (2,582,116 = leaf-selected mixer; one-set = D1 = 2,583,403). I ran both; new signals were added on the one-set variant (no multi-set mixer reintroduced). Compare against both.

## Signals implemented (all causal, integer; refine only the 6 "continuous" leaves 6..11 via a sub-class, flat/two-level/smooth leaves unchanged)
Texture identity (pixel relationships, not values):
- **Z** zigzag: sign flips (0/1/2) of the three differences along the row above (ramp/edge vs fine oscillation).
- **K** curvature: |(W-WW)-(N-NW)| + |(W-NW)-(N-NN)| vs mean gradient (planar / mixed / rough). Measures whether the gradient *repeats* across neighbours.
Spatial scale (only from fully coded rows; per-column EMA, then horizontal box average; x16 fixed point):
- **RES** local residual energy vs medium (±3 cols, ~4 rows) residual energy (outlier / similar / quiet pocket).
- **ACT** local pixel activity vs medium activity.
- **DIR** medium-scale orientation: horizontal vs vertical activity (H / V / isotropic).
- **BRD** broad (±12 cols, ~16 rows) activity level (3 bands).

## Results (5 reps, bytes; total / vs production / vs D1 / vs BEST)
| config | total | vs prod | vs D1 | vs BEST | portrait | screen | night |
|---|---|---|---|---|---|---|---|
| production | 2,612,940 | 0 | +1.14% | +1.19% | 481,178 | 222,580 | 1,909,182 |
| 1. REF-BEST (leaf mixer) | 2,582,116 | -1.180% | -0.050% | 0 | 468,634 | 210,203 | 1,903,279 |
| 1. REF-D1 (one set) | 2,583,403 | -1.130% | 0 | +0.050% | 469,145 | 210,285 | 1,903,973 |
| 2. texture: K | 2,582,507 | -1.165% | -0.035% | +0.015% | 469,002 | 209,545 | 1,903,960 |
| 2. texture: Z+K | 2,582,470 | -1.166% | -0.036% | +0.014% | 468,987 | 209,293 | 1,904,190 |
| 3. scale: DIR | 2,582,865 | -1.151% | -0.021% | +0.029% | 469,097 | 209,663 | 1,904,105 |
| 4. combined: K+DIR | 2,582,153 | -1.178% | -0.048% | +0.001% | 469,000 | 208,788 | 1,904,365 |

Other single signals (1 rep, `screen_a.log`, total vs D1): Z -0.013%, RES -0.023%, ACT -0.017%, BRD -0.018%. Larger combos (`screen_b.log`): DIR+BRD -0.028%, RES+DIR+BRD -0.025%, Z+K+DIR -0.036%, Z+K+DIR+BRD (81 sub-classes) -0.016%. Bigger combinations were not better; they diluted contexts.

## Which signals carry new information
- **K (curvature)** is the only texture signal with a real per-image effect: screen -0.35%, portrait -0.03%, night 0. Z adds nothing beyond K.
- **DIR (medium orientation)** helps screen (-0.30%) only.
- RES, ACT, BRD duplicate what the tree/immediate context already encodes (each ≤0.02%): rejected.
- Night (the largest image, 73% of bytes) gets slightly *worse* (+0.01..0.02%) with every added signal: the continuous leaves are already 77% of its pixels and further splitting only dilutes statistics. Portrait/screen benefit where the content has planar gradients and axis-aligned structure, but those images are small.

## Cost (5-rep best, 3-image sums, noisy ±15%)
Enc / dec ms: production 2,010 / 1,614; D1 1,774 / 2,747 (noisy); K+DIR 3,032 / 3,801. Classification overhead measured with the signal computed but unused: tree only 2,094 enc, K+DIR 2,786 enc (+~33%, region statistics add a per-row pass and per-pixel curvature). Metadata: 0 tree bytes + 1 mode byte per file (3 total, included). Leaves go 12 → up to 6+6·9 = 60 for K+DIR.

## Correctness
Every config of every run: pixel-exact round trip and deterministic re-encode (all `exact=true det=true`; checked in all three logs). All signals use only pixels/residuals from before the current sample (or whole prior rows); encoder and decoder share the single scan.

## Decision
**Reject (do not retain).** The best result (K+DIR, -0.048% vs D1, 1 byte of difference vs REF-BEST: +0.001%) is the size of the earlier simplification noise, was found by selection on three images, helps only the two small images, hurts night, and costs ~+30% encode time and ~5x more leaves. It does not clear the bar of reducing bytes enough to justify the complexity. The leaf-mixer REF-BEST remains the best (2,582,116) and gets the same gain for free.

Interpretation: with pixel-value diversity, predictability, and directional coherence already in the tree, texture organisation (curvature, oscillation) and larger-scale context add ~0.04% at best. The probability model's residual-energy contexts (B, C) already absorb the medium-scale residual information (RES, BRD). The remaining gap is more likely in the predictor than in classification, but that is out of scope here.

Caveats: three images; thresholds were fixed up front (not swept) so a tuned variant could do somewhat better, but gains of this size would need the 50-image set to confirm, which was not run. The auto-generated `summary.md` tables have a column-alignment bug in the "vs D1" column; the numbers above come from `per_image.csv`.
