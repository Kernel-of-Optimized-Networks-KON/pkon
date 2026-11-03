# Range decoder verbatim-copy control

Follows `range-decoder-controlled-experiment.md` (untouched). Raw data: `raw/rc3/` (`verify_3img.txt`, `time_<image>.txt`; `T rep variant seconds unix_ms`, `W` = warm-up, excluded).

## Question
Is B's 3-9% gain over A due to branch-free resolution, or to compiling in the harness crate rather than the library?

## Implementation comparison
- **A**: library `Stage4Dec` (`src/ellie.rs:1346-1387`), driven by the harness.
- **K (A-copy)**: `rc::Copy0` in `harness/src/rc.rs` (existing since the quick check; unmodified). Line-for-line the same logic as `Stage4Dec::code`/`new`: `bound = (range*p1)>>16`, `if code < bound {range=bound; 1} else {code-=bound; range-=bound; 0}`, `while range < 1<<24` refill with `buf.get(pos).unwrap_or(0)`. Differences: crate location, `pub` fields, local `TOP` const (= `S4_TOP`, 1<<24). Same `#[inline(always)]`.
- **B**: `rc::Bf` (branch-free mask resolve, same refill), unchanged.

## Commands
```
cd benchmarks/ulisa-decode-profile/harness && cargo build --release      # adds src/bin/rcctl.rs
rcctl verify <leica_m10_brick|parrot_anafi_dusk|sigma_fp_bike8>.ulisa7.pkon
rcctl time <file> <reps> <cooldown> 1   # brick 12 reps/3 s, parrot 12/3 s, bike8 18/2 s, 1 warm-up round
# each job pinned to CPU 4 (ProcessorAffinity=1<<4), PriorityClass High, one job at a time (scratchpad run3.ps1)
```
Order: the 6 permutations of (A,K,B) cycled per repetition (each variant in each position equally often; brick/parrot = 2 cycles, bike8 = 3).

## Correctness
A, K, B on all 3 images: samples identical, pixel CRC equal, bytes consumed identical (26298803 / 11569253 / 2660993). All OK, no errors/fallbacks (`raw/rc3/verify_3img.txt`); every timed decode was re-asserted.

## Timing (seconds)
| image | var | median | min | max | CV |
|---|---|---|---|---|---|
| brick | A | 6.301 | 6.247 | 6.793 | 2.5% |
| | K | 6.305 | 6.227 | 6.742 | 2.3% |
| | B | 5.749 | 5.677 | 5.961 | 1.6% |
| parrot | A | 3.123 | 3.028 | 6.516 | 26.9% |
| | K | 3.100 | 3.019 | 4.752 | 18.4% |
| | B | 2.966 | 2.890 | 4.491 | 15.6% |
| bike8 | A | 0.809 | 0.801 | 1.168 | 9.9% |
| | K | 0.811 | 0.797 | 0.973 | 5.3% |
| | B | 0.789 | 0.778 | 1.102 | 9.4% |

Paired ratios (median [range], variant lower in n reps):
| image | K/A | B/A | B/K |
|---|---|---|---|
| brick | 0.996 [0.954, 1.070] 7/12 | 0.909 [0.870, 0.943] 12/12 | 0.913 [0.850, 0.956] 12/12 |
| parrot | 1.001 [0.729, 1.367] 5/12 | 0.948 [0.689, 1.226] 9/12 | 0.950 [0.826, 1.225] 10/12 |
| bike8 | 1.002 [0.722, 1.194] 9/18 | 0.971 [0.935, 1.102] 14/18 | 0.978 [0.812, 1.307] 13/18 |

Min-vs-min: K vs A -0.3% / -0.3% / -0.5%; B vs K -8.8% / -4.3% / -2.4%. Parrot and bike8 had bursts of desktop interference this run (max up to 2x median, large CV); their medians and minimums are stable, the spread is not. Raw outliers are preserved.

## Decision: 1. Control passes
- A vs K: medians within 0.4% on every image, mins within 0.5%, paired wins split ~50/50. No crate/codegen offset.
- B vs K (same harness context): 8.7% / 5.0% / 2.2% faster by median, 35/42 paired wins; brick is 12/12 with a tight range. The gain from the previous experiment survives.
- Caveat: parrot and bike8 are noisier this time; the claim rests mainly on brick (clean) and on the agreement of three independent sets of medians/minimums with the previous run (9.1/5.4/2.9% then, 8.7/5.0/2.2% now).

Next: consider the minimal production port of B only into `Stage4Dec::code` (no format change), then one alternating production-build A/B bench on brick and bike8. Nothing in production has been changed.
