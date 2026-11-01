# ELLIE Paeth production three-image benchmark

Production `nanoscope::pkon` encode/decode, release, auto representation. Baseline = HEAD (original Stage 1 diagonal) built in a separate worktree; Paeth = working tree. Same 1024-px centre crops as `benchmarks/ellie-final-3img`. 6 interleaved A/B rounds (alternating order), each worker = best of 2 reps; table timings = min over the 6 rounds (median in brackets). Encode = auto (both representations tried); YCoCg-R-forced encode also shown.

| image | repr | raw B | base file B | paeth file B | delta B | delta % | ratio base | ratio paeth |
|---|---|---|---|---|---|---|---|---|
| portrait_0 | YCoCg-R/YCoCg-R | 3145728 | 548071 | 500597 | -47474 | -8.662% | 5.740 | 6.284 |
| screen_0 | YCoCg-R/YCoCg-R | 4194304 | 311035 | 285917 | -25118 | -8.076% | 13.485 | 14.670 |
| night_0 | YCoCg-R/YCoCg-R | 3145728 | 1976665 | 1947077 | -29588 | -1.497% | 1.591 | 1.616 |
| **total** | | 10485760 | 2835771 | 2733591 | -102180 | -3.603% | 3.698 | 3.836 |

Timings, seconds (min over rounds [median]):

| image | enc auto base | enc auto paeth | enc ycocg base | enc ycocg paeth | dec base | dec paeth |
|---|---|---|---|---|---|---|
| portrait_0 | 0.385 [0.398] | 0.385 [0.392] | 0.165 [0.178] | 0.165 [0.168] | 0.195 [0.226] | 0.194 [0.197] |
| screen_0 | 0.453 [0.509] | 0.463 [0.477] | 0.205 [0.287] | 0.210 [0.223] | 0.238 [0.381] | 0.234 [0.270] |
| night_0 | 1.250 [1.680] | 1.548 [1.644] | 0.542 [0.807] | 0.718 [0.850] | 0.654 [0.894] | 0.768 [0.925] |
| **sum of mins** | 2.089 | 2.396 | 0.913 | 1.093 | 1.088 | 1.196 |

Sum-of-min change (paeth vs base): encode auto +14.7%, encode YCoCg-R +19.8%, decode +10.0%. Run-to-run noise is large (single cells vary up to ~2-3x between rounds from system load), so timing differences of this size are not conclusive.

Reference: previous untransformed baseline 3,572,255 B; YCoCg-R baseline 2,835,771 B (reproduced exactly: True); experimental Paeth estimate 2,733,591 B (production matches: True).

Correctness: pixel-identical round trip, spec/layout/header match, on-disk .pkon re-decode, and in-process repeat-encode determinism (auto == YCoCg-R-forced == repeats), all configs and all 6 rounds: True. Cross-process determinism: file sizes identical in all 6 rounds. Raw total 10485760 B equals canonical 10,485,760.
