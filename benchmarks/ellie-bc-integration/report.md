# ELLIE Stage 3 profile production A/B: Legacy4 vs Bc255S11

Harness: [bench_ellie_profile_ab.rs](bench_ellie_profile_ab.rs) (`bench_ellie_profile_ab --set 3|50 --reps N --out <dir>`).
Both profiles run through the real `.pkon` container (`encode_standard_profile(.., Auto, ..)` / `decode`), on 1024-px centre crops,
interleaved with rotating order. Time per image is the minimum over repetitions. The `--reps` value used was not recorded.

## Recovered artifacts

These are the original outputs, copied unmodified (SHA-256 verified) from the session scratchpad
`/tmp/claude/c--Dev-GitHub-pkon/03943e9d-ee99-44c2-a39c-147d13a368ca/scratchpad/` (run finished Nov 1 2026, 02:41 for 3 images and 02:49 for 50).

| Path | Contents |
|---|---|
| [ab50/results.csv](ab50/results.csv) | 50 images x 2 profiles: sizes, min encode/decode time, exact, deterministic, representation |
| [ab50/legacy4/](ab50/legacy4/), [ab50/bc255s11/](ab50/bc255s11/) | 50 `.pkon` files each |
| [ab50.err](ab50.err) | per-image size log from stderr |
| [ab3/results.csv](ab3/results.csv) | 3-image run (portrait_0, screen_0, night_0) |
| [ab3/legacy4/](ab3/legacy4/), [ab3/bc255s11/](ab3/bc255s11/) | 3 `.pkon` files each |

## 50-image result (recomputed from the copied CSV and `.pkon` files)

| Metric | Legacy4 | Bc255S11 |
|---|---:|---:|
| Total bytes | 43,580,981 | 43,453,888 |
| Encode time (sum of per-image minimums) | 36.20 s | 30.30 s |
| Decode time (sum of per-image minimums) | 18.31 s | 15.25 s |
| Not bit-exact | 0 / 50 | 0 / 50 |
| Non-deterministic | 0 / 50 | 0 / 50 |
| Representation chosen by Auto | YCoCg-R on 50 / 50 | YCoCg-R on 50 / 50 |

Byte totals from `results.csv` equal the summed sizes of the `.pkon` files on disk, and every per-file size in the CSV equals the file's size.

## Not recorded

The `--reps` value, any stdout log or manifest beyond `ab50.err`, and the size of the untransformed alternative for each image
(the harness records only the representation Auto selected).
