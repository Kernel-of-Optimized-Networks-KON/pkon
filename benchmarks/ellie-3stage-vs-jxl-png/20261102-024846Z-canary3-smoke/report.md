# ELLIE (3-stage, profile 2) vs JPEG XL and PNG: 3-image canary smoke test

Images: 3; valid (decoded, pixel-identical, spec-identical) for all codecs: 3; invalid records: 0.

Raw (1024x1024 centre crops; portrait_0 and night_0 RGB8, screen_0 RGBA8): 10485760 B, 3145728 px. Ratio = raw / stored for every codec.

## Aggregate (sum of stored bytes)

| codec | total bytes | bpp | aggregate ratio | ELLIE size vs codec |
|---|---|---|---|---|
| ellie | 2612940 | 6.645 | 4.013 | 0.0% |
| jxl-e7-t1 | 2401029 | 6.106 | 4.367 | +8.83% |
| jxl-e9-t1 | 2394345 | 6.089 | 4.379 | +9.13% |
| png-rs | 3399915 | 8.646 | 3.084 | -23.15% |

(Negative = ELLIE smaller.)

## Timing (sum of best-of-2, ms; net = minus per-call process floor for ffmpeg codecs; ELLIE/PNG in-process)

| codec | enc total | enc net | enc MP/s net | enc ms/img | dec total | dec net | dec MP/s net | dec ms/img |
|---|---|---|---|---|---|---|---|---|
| ellie | 2219 | 2219 | 1.42 | 740 | 1278 | 1278 | 2.46 | 426 |
| jxl-e7-t1 | 3513 | 3365 | 0.93 | 1122 | 1473 | 1317 | 2.39 | 439 |
| jxl-e9-t1 | 14854 | 14670 | 0.21 | 4890 | 1340 | 1171 | 2.69 | 390 |
| png-rs | 1293 | 1293 | 2.43 | 431 | 50 | 50 | 62.47 | 17 |

## Per-image (bytes, ratio, ELLIE vs codec; ms enc/dec)

| image | ellie B (ratio) | jxl-e7 B | jxl-e9 B | png B | vs e7 | vs e9 | vs png | ellie ms e/d | e7 ms e/d | e9 ms e/d | png ms e/d |
|---|---|---|---|---|---|---|---|---|---|---|---|
| portrait_0 | 481178 (6.54) | 398485 (7.89) | 390589 (8.05) | 880271 (3.57) | +20.8% | +23.2% | -45.3% | 560/266 | 914/472 | 3793/489 | 875/11 |
| screen_0 | 222580 (18.84) | 121247 (34.59) | 112699 (37.22) | 229481 (18.28) | +83.6% | +97.5% | -3.0% | 665/473 | 574/275 | 1755/254 | 133/5 |
| night_0 | 1909182 (1.65) | 1881297 (1.67) | 1891057 (1.66) | 2290163 (1.37) | +1.5% | +1.0% | -16.6% | 994/540 | 2025/726 | 9306/597 | 285/34 |

