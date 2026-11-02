# Structural tree x probability-model simplification

Continues `../` (untouched). Source: `exp_ellie_tree_simplify.rs` (copy of the previous harness plus model knobs). Logs: `screen.log` (round 1, 1 rep), `round2.log` (3 reps), `final.log` (5 reps); tables in `results*/summary.md`. Canary, release, complete-file bytes (production container overhead + streams + 1 mode byte when the tree influences coding). Production code untouched; only `Cargo.toml` got one more `[[bin]]`. Not committed.

Sanity: `prod` is byte-identical to production (asserted); `prevbest` reproduces **2,582,588** exactly. All configs: pixel-exact round trip and deterministic re-encode (every row in the logs).

## Answers by component (aggregate vs production 2,612,940; tree = leaf as T context, no stationarity, no per-leaf Shrub)
| question | with tree | without tree | decision |
|---|---|---|---|
| A. contexts: B+C+T (full) | -1.162% | 0 (B+C) | reference |
| A. B+T only (drop C) | -0.834% | +0.452% | C still needed |
| A. C+T only (drop B) | **-1.067%** | +1.990% (C only) | B is mostly replaced: costs 0.10% |
| A. T only | -0.587% | +0.602% (qS only) | too far |
| B. mixer: 1 weight set instead of leaf/qS sets | -1.121% | +0.033% | selection worth only 0.04%; tree can't replace the mixer |
| B. mixer fixed average (no learning) | -0.601% | +0.482% | learning mixer is necessary (0.56%) |
| C. remove count-based confidence scaling | **-1.180%** | +0.011% | redundant; removing is free or slightly better |
| C. count cap 60 / 30 | -1.010% / -0.771% | +0.160% / +0.411% | adaptation depth still needed (helps screen/portrait, hurts night) |

## Combined (round 2/final, no confidence scaling + single mixer set)
| config | total B | vs prod | vs prev tree | portrait | screen | night |
|---|---|---|---|---|---|---|
| production | 2,612,940 | 0 | +1.18% | 481,178 | 222,580 | 1,909,182 |
| previous tree best (B,C,T + leaf mixer) | 2,582,588 | -1.162% | 0 | 468,572 | 210,006 | 1,904,010 |
| **BEST: B,C,T, no confidence scaling, leaf mixer** | **2,582,116** | **-1.180%** | -0.018% | 468,634 | 210,203 | 1,903,279 |
| D1: B,C,T, no confidence, 1 mixer set | 2,583,403 | -1.130% | +0.031% | 469,145 | 210,285 | 1,903,973 |
| **D3: C,T only, no confidence, 1 mixer set** | 2,584,603 | -1.084% | +0.078% | 470,661 | 210,529 | 1,903,413 |
| D2: B,T, no confidence, 1 set | 2,590,688 | -0.852% | | | | |
| same simplifications, no tree (B,C) | 2,614,065 | +0.043% | | 481,904 | 222,796 | 1,909,365 |

Round-2 note: the leaf mixer and the single set differ by only 0.03%.

## Complexity
Production: contexts B (12,288 slots) + C (16,384) active, 4 mixer weight sets, 3 inputs + bias, confidence table. Tree adds T (up to 32,768 nominal slots, ~12 leaves x 32 qS used).
- D3 uses **2 context tables (C 16,384 + T), 1 mixer weight set, no confidence table, no B**: one fewer context and 3 fewer mixer sets than production, at -1.08% instead of 0. Per-bit work is 2 slot lookups/updates instead of 2, so speed is production-like in principle, but the tree classifier still runs.
- Timing (best of 5, noisy, +/-15%, 3-image sums): production harness enc 1,787 / dec 2,017 ms; prevbest 2,317 / 2,754; BEST 2,170 / 2,390; D1 2,195 / 2,276; D3 2,155 / 2,135; no-tree simplified 1,825 / 1,691. Simplified+tree is ~+20% enc, ~+5% dec over production; not a speed win. Metadata: 0 tree bytes + 1 mode byte (3 over the canary, included).

## Conclusions
1. The experiment ran; all numbers are real encoded bytes with round-trip and determinism verified.
2. Tree genuinely replaced some complexity: context **B** (qS x |aW-aN|) becomes nearly redundant (cost 0.10% when dropped with the tree, vs a +0.45% to +2% loss for dropping a context without it), and the **confidence scaling** is redundant with or without the tree.
3. Third condition: the same simplifications without the tree are neutral at best (+0.04%), and contexts removed without it hurt a lot. So the benefit of removing B comes from the tree, not from a better simplified model.
4. The tree did **not** replace the mixer's learning (fixed average loses 0.56%), C, or adaptation depth. Weight-set selection (by qS or by leaf) is only worth ~0.04%, so the 4 sets could go in any setting.
5. Net: best measured is -1.180% (only 0.018% better than the previous best, from deleting the confidence table). The tradeoff frontier: D1 (-1.13%, single mixer set), D3 (-1.08%, one context table fewer). The gains from simplification are removed parts, not byte savings.
6. Caveat: three images, selection on the canary, timing noisy; no held-out check done.

## Best measured configuration
`BEST noconf (B,C,T, leaf-mixer)`: source `benchmarks/ellie-structural-tree/simplify/exp_ellie_tree_simplify.rs`, run with `SET=f` (config defined in `configs()`): `conf: false` on top of the previous best (`tr("", true, true)`). Simplest recommended: `D3` in the same file.
