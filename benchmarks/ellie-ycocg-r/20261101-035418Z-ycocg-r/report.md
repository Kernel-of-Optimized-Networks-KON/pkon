# ELLIE YCoCg-R experiment

Release, best of reps. Bytes = Stage 4 payload (headerless). YCoCg-R: Y 8-bit codec, Co/Cg 9-bit codec (stored +255), alpha unchanged; one Stage 4 stream per plane (Stage4Dec is private to decode_planes, so codecs of different bit depth cannot share one stream; costs a few termination bytes per extra stream).

| image | baseline B | YCoCg-R B | delta B | delta % | ratio base | ratio ycocg | planes (ycocg) |
|---|---|---|---|---|---|---|---|
| portrait_0 | 987160 | 548022 | -439138 | -44.48% | 3.187 | 5.740 | [320681, 126556, 100785] |
| screen_0 | 527075 | 310981 | -216094 | -41.00% | 7.958 | 13.487 | [178329, 81191, 51360, 101] |
| night_0 | 2057866 | 1976616 | -81250 | -3.95% | 1.529 | 1.591 | [665589, 710086, 600941] |
| **aggregate** | 3572101 | 2835619 | -736482 | -20.62% | 2.935 | 3.698 | |

Baseline expected 3572101: REPRODUCED.

Timing (sum over images, best-of-reps each): encode base 1.070s / ycocg 0.957s; decode base 1.203s / ycocg 1.373s; transform fwd 0.0072s, inverse 0.0093s (included in ycocg totals).
Round trip all exact: true. Deterministic: true.

## Production container (codec 6)

| image | mode | file B | header+meta B | payload B | encode s | decode s | |
|---|---|---|---|---|---|---|---|
| portrait_0 | untransformed | 987210 | 49 | 987161 | 0.458 | 0.553 | repr 0 |
| portrait_0 | ycocg-r | 548071 | 49 | 548022 | 0.324 | 0.334 | repr 1 |
| portrait_0 | auto | 548071 | 49 | 548022 | 0.728 | 0.355 | repr 1 |
| screen_0 | untransformed | 527130 | 54 | 527076 | 0.463 | 0.531 | repr 0 |
| screen_0 | ycocg-r | 311035 | 54 | 310981 | 0.377 | 0.427 | repr 1 |
| screen_0 | auto | 311035 | 54 | 310981 | 0.863 | 0.491 | repr 1 |
| night_0 | untransformed | 2057915 | 49 | 2057866 | 0.730 | 0.875 | repr 0 |
| night_0 | ycocg-r | 1976665 | 49 | 1976616 | 0.628 | 0.739 | repr 1 |
| night_0 | auto | 1976665 | 49 | 1976616 | 1.402 | 0.830 | repr 1 |

Production file totals: untransformed 3572255 B, ycocg-r 2835771 B, auto 2835771 B.
