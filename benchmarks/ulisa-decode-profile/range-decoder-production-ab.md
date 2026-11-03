# Range decoder: production A/B verification

Follows `range-decoder-verbatim-control.md`. Raw data: `raw/rc4/` (`verify_pre.txt`, `verify_post.txt`, `time_<image>.txt`: `T pair build seconds`).

## Builds
- **A**: the unmodified repo library (`src/`, git-clean), linked into `prodab/benchA`.
- **B**: `prodab/libB`, a copy of `src/` + `build.rs` + `Cargo.lock` with only `Stage4Dec::code` changed (branch-free resolve; diff vs `src/ellie.rs` is exactly the 8-line resolve block, the refill loop is untouched). Linked into `prodab/benchB`. Discard by deleting `prodab/`.
- Same bench source `prodab/bench/main.rs` for both; uses only public `pkon::decode` (end-to-end production path, pixel CRC checked inside). Same release profile as the repo.

## Method
Per sample: a fresh process = 1 untimed warm-up decode then 1 timed decode. Pair order alternates (A,B / B,A), 3 s idle before each process (2 s bike8), pinned to CPU 4, High priority, one process at a time. 10 pairs brick, 16 pairs bike8. Driver: scratchpad `run4.ps1`; build: `cargo build --release` in `benchA`/`benchB`.

## Correctness (before and after timing, identical)
| image | build | sample hash (FNV-1a) | pixel CRC | stored CRC |
|---|---|---|---|---|
| brick | A, B | 1594dd1cca00d124 | 6908664b | 6908664b |
| bike8 | A, B | ded089ff4aecf47b | e7a04a8d | e7a04a8d |
A and B are sample-for-sample identical, CRC matches, no errors/fallback. **Not verified in production builds:** payload bytes consumed. `Stage4Dec::consumed()` is not reachable through the public decode API and I did not alter A to expose it. It was verified identical for the same arithmetic in the harness (rc2/rc3), and identical outputs over the full stream make a difference implausible.

## Timing (seconds, every measurement)
brick A: 9.459 7.157 9.796 11.795 10.310 7.214 7.134 8.121 12.815 8.161
brick B: 6.785 6.656 9.147 9.496 6.745 6.755 7.450 9.230 9.121 7.709
bike8 A: 0.901 0.915 1.018 1.114 1.020 0.920 0.885 0.882 0.877 0.883 0.915 0.887 0.888 1.181 3.770 0.971
bike8 B: 0.865 0.865 1.289 1.212 0.921 0.940 0.863 0.863 0.881 0.856 0.905 0.920 0.905 1.045 0.962 0.942

| image | build | median | min | max | CV | paired B/A median [range] | B faster | min/min |
|---|---|---|---|---|---|---|---|---|
| brick | A | 8.810 | 7.134 | 12.815 | 20.7% | | | |
| | B | 7.579 | 6.656 | 9.496 | 14.4% | 0.932 [0.654, 1.136] | 8/10 | 0.933 |
| bike8 | A | 0.915 | 0.877 | 3.770 | 61% | | | |
| | B | 0.912 | 0.856 | 1.289 | 13% | 0.977 [0.255, 1.265] | 10/16 | 0.976 |

## Assessment
- Direction reproduces on the real production path: B is faster at the median, minimum and in most pairs on both images (-6.7% / -2.4% min-vs-min), same sign and similar size as the harness result (-9% / -3%).
- But this run was much noisier than rc2/rc3: A on brick ranged 7.1–12.8 s (earlier sessions 6.2–6.8 s). Processes were also short-lived (cold start per sample) and the desktop was not quiet. Win counts (8/10, 10/16) and medians are therefore weak evidence by themselves; the min-vs-min numbers and the agreement with two earlier clean sessions carry the case.
- Verdict: **directionally confirmed, not cleanly reproduced.** No correctness regression. The gain does not contradict the harness result, but this dataset alone would not meet a "repeatable" bar.

## Recommendation (needs approval; nothing ported)
Smallest follow-up to firm it up: rerun this exact A/B on `leica_m10_brick` only, 12 pairs, with the desktop quiet (browsers closed) and 2 timed decodes per process after the warm-up, or equivalently once the machine shows A ≈ 6.3 s. If B/A stays ≈0.91-0.93, port the 3-line change into `Stage4Dec::code`. Given correctness is exact and the harness evidence is already strong (rc2 + rc3 clean runs), porting now is also defensible; that is your call.
