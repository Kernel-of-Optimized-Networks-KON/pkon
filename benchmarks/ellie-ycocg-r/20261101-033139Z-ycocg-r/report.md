# ELLIE YCoCg-R experiment

Release, best of reps. Bytes = Stage 4 payload (headerless). YCoCg-R: Y 8-bit codec, Co/Cg 9-bit codec (stored +255), alpha unchanged; one Stage 4 stream per plane (Stage4Dec is private to decode_planes, so codecs of different bit depth cannot share one stream; costs a few termination bytes per extra stream).

| image | baseline B | YCoCg-R B | delta B | delta % | ratio base | ratio ycocg | planes (ycocg) |
|---|---|---|---|---|---|---|---|
| portrait_0 | 987160 | 548022 | -439138 | -44.48% | 3.187 | 5.740 | [320681, 126556, 100785] |
| screen_0 | 527075 | 310981 | -216094 | -41.00% | 7.958 | 13.487 | [178329, 81191, 51360, 101] |
| night_0 | 2057866 | 1976616 | -81250 | -3.95% | 1.529 | 1.591 | [665589, 710086, 600941] |
| **aggregate** | 3572101 | 2835619 | -736482 | -20.62% | 2.935 | 3.698 | |

Baseline expected 3572101: REPRODUCED.

Timing (sum over images, best-of-reps each): encode base 1.569s / ycocg 1.358s; decode base 1.740s / ycocg 1.863s; transform fwd 0.0099s, inverse 0.0102s (included in ycocg totals).
Round trip all exact: true. Deterministic: true.
