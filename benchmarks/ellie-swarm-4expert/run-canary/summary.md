# ELLIE four-expert swarm: canary results

Release build, 1024-px centre crops, representation chosen by production Auto. Sizes = production container overhead + real Stage 3/4 streams (+1 mode byte for every swarm config). Production total: 2612940 B (portrait_0 481178, screen_0 222580, night_0 1909182). Raw total 10485760 B. Timings: best / median of 5 runs, single thread, in-process.

## Reproduction gate

```
portrait_0 plane 0: prediction plane and residuals identical to live Stage1 (1048576 px)
portrait_0 plane 1: prediction plane and residuals identical to live Stage1 (1048576 px)
portrait_0 plane 2: prediction plane and residuals identical to live Stage1 (1048576 px)
screen_0 plane 0: prediction plane and residuals identical to live Stage1 (1048576 px)
screen_0 plane 1: prediction plane and residuals identical to live Stage1 (1048576 px)
screen_0 plane 2: prediction plane and residuals identical to live Stage1 (1048576 px)
screen_0 plane 3: prediction plane and residuals identical to live Stage1 (1048576 px)
night_0 plane 0: prediction plane and residuals identical to live Stage1 (1048576 px)
night_0 plane 1: prediction plane and residuals identical to live Stage1 (1048576 px)
night_0 plane 2: prediction plane and residuals identical to live Stage1 (1048576 px)
```
prod-copy streams byte-identical to the production payload and total file size equal to production on all three images (asserted).

## Bytes

| config | total B | vs prod (B) | vs prod % | portrait B (%) | screen B (%) | night B (%) | ratio raw/file | round-trip | deterministic |
|---|---|---|---|---|---|---|---|---|---|
| prod-copy | 2612940 | +0 | +0.000% | 481178 (+0.00%) | 222580 (+0.00%) | 1909182 (+0.00%) | 4.0130 | exact | true |
| P4-prodrel | 2633408 | +20468 | +0.783% | 489126 (+1.65%) | 220676 (-0.86%) | 1923606 (+0.76%) | 3.9818 | exact | true |
| P4-shared | 2642665 | +29725 | +1.138% | 492611 (+2.38%) | 223514 (+0.42%) | 1926540 (+0.91%) | 3.9679 | exact | true |
| P4-shared-tree5 | 2620383 | +7443 | +0.285% | 470255 (-2.27%) | 221360 (-0.55%) | 1928768 (+1.03%) | 4.0016 | exact | true |
| P4-shared-tree9 | 2621571 | +8631 | +0.330% | 470223 (-2.28%) | 221335 (-0.56%) | 1930013 (+1.09%) | 3.9998 | exact | true |
| P4-shared-Lpx | 2625595 | +12655 | +0.484% | 479829 (-0.28%) | 221189 (-0.62%) | 1924577 (+0.81%) | 3.9937 | exact | true |

## Timing (ms, sum over the 3 images; best / median)

Production `pkon` single-representation encode 1564 ms, decode 1519 ms (best). Harness configs use safe indexing and an unfused Stage1->Stage3 split, so compare within the table (prod-copy is the reference). scan-only = prediction/reliability/tree work with no entropy coding.

| config | encode | decode | scan-only | enc vs prod-copy | dec vs prod-copy |
|---|---|---|---|---|---|
| prod-copy | 1457 / 2065 | 1673 / 2068 | 626 | +0.0% | +0.0% |
| P4-prodrel | 1688 / 1848 | 1829 / 1977 | 788 | +15.9% | +9.3% |
| P4-shared | 1655 / 1950 | 2309 / 2518 | 1066 | +13.6% | +38.0% |
| P4-shared-tree5 | 2631 / 2821 | 2581 / 3461 | 1539 | +80.6% | +54.3% |
| P4-shared-tree9 | 2418 / 2851 | 2357 / 2652 | 1364 | +66.0% | +40.9% |
| P4-shared-Lpx | 1736 / 1899 | 1907 / 2066 | 786 | +19.1% | +14.0% |

Tree/state setup (Scan buffers + Pred state for a 1024-wide plane): 0.5 us. The tree itself has no construction phase: leaves are classified inline per pixel from causal pixels, no stored thresholds.

## Verification (encoder + decoder verification pass, all planes)

| image | config | pixels | enc/dec state-sync | u128 mismatches | bound violations | overflow | negative estimates | clamp hits |
|---|---|---|---|---|---|---|---|---|
| portrait_0 | P4-prodrel | 3142656 | true | 0 | 0 | 0 | 0 | 0 |
| portrait_0 | P4-shared | 3142656 | true | 0 | 0 | 0 | 0 | 0 |
| portrait_0 | P4-shared-tree5 | 3142656 | true | 0 | 0 | 0 | 0 | 0 |
| portrait_0 | P4-shared-tree9 | 3142656 | true | 0 | 0 | 0 | 0 | 0 |
| portrait_0 | P4-shared-Lpx | 3142656 | true | 0 | 0 | 0 | 0 | 0 |
| screen_0 | P4-prodrel | 4190208 | true | 0 | 0 | 0 | 0 | 5181 |
| screen_0 | P4-shared | 4190208 | true | 0 | 0 | 0 | 0 | 5218 |
| screen_0 | P4-shared-tree5 | 4190208 | true | 0 | 0 | 0 | 0 | 5183 |
| screen_0 | P4-shared-tree9 | 4190208 | true | 0 | 0 | 0 | 0 | 5060 |
| screen_0 | P4-shared-Lpx | 4190208 | true | 0 | 0 | 0 | 0 | 4322 |
| night_0 | P4-prodrel | 3142656 | true | 0 | 0 | 0 | 0 | 1350 |
| night_0 | P4-shared | 3142656 | true | 0 | 0 | 0 | 0 | 966 |
| night_0 | P4-shared-tree5 | 3142656 | true | 0 | 0 | 0 | 0 | 833 |
| night_0 | P4-shared-tree9 | 3142656 | true | 0 | 0 | 0 | 0 | 779 |
| night_0 | P4-shared-Lpx | 3142656 | true | 0 | 0 | 0 | 0 | 592 |

## Shared reliability behaviour (mean per-pixel normalised weight share / mean global g_i)

| image | config | H | V | D' | Paeth | g: H / V / D' / Paeth |
|---|---|---|---|---|---|---|
| portrait_0 | P4-prodrel | 20.4% | 28.9% | 22.6% | 28.1% | 142 / 179 / 110 / 241 |
| portrait_0 | P4-shared | 22.1% | 27.1% | 23.1% | 27.6% | 163 / 183 / 118 / 250 |
| portrait_0 | P4-shared-tree5 | 21.6% | 26.7% | 24.8% | 26.9% | 163 / 183 / 118 / 250 |
| portrait_0 | P4-shared-tree9 | 21.6% | 26.7% | 24.8% | 26.9% | 163 / 183 / 118 / 250 |
| portrait_0 | P4-shared-Lpx | 17.7% | 21.7% | 18.0% | 42.5% | 163 / 183 / 118 / 250 |
| screen_0 | P4-prodrel | 23.9% | 26.3% | 24.4% | 25.4% | 228 / 239 / 221 / 245 |
| screen_0 | P4-shared | 23.5% | 26.7% | 23.3% | 26.5% | 219 / 235 / 207 / 245 |
| screen_0 | P4-shared-tree5 | 23.7% | 25.8% | 24.9% | 25.7% | 219 / 235 / 207 / 245 |
| screen_0 | P4-shared-tree9 | 23.7% | 25.8% | 24.9% | 25.6% | 219 / 235 / 207 / 245 |
| screen_0 | P4-shared-Lpx | 23.0% | 26.0% | 22.8% | 28.1% | 219 / 235 / 207 / 245 |
| night_0 | P4-prodrel | 28.8% | 25.7% | 42.4% | 3.1% | 178 / 171 / 158 / 200 |
| night_0 | P4-shared | 29.2% | 25.3% | 42.2% | 3.3% | 191 / 179 / 160 / 226 |
| night_0 | P4-shared-tree5 | 29.0% | 24.8% | 42.9% | 3.3% | 191 / 179 / 160 / 226 |
| night_0 | P4-shared-tree9 | 28.1% | 24.3% | 44.4% | 3.2% | 191 / 179 / 160 / 226 |
| night_0 | P4-shared-Lpx | 21.2% | 18.1% | 34.8% | 25.9% | 191 / 179 / 160 / 226 |

## Tree modulation, P4-shared-tree5: leaf share and mean mu (Q8, 256 = neutral) per expert, all images pooled

| leaf | pixels % | mu H | mu V | mu D' | mu Paeth |
|---|---|---|---|---|---|
| 0 | 47.7% | 293 | 261 | 396 | 237 |
| 1 | 13.2% | 298 | 411 | 177 | 507 |
| 2 | 11.4% | 252 | 286 | 312 | 262 |
| 3 | 12.0% | 258 | 286 | 271 | 284 |
| 4 | 15.8% | 277 | 253 | 274 | 258 |

## Tree modulation, P4-shared-tree9: leaf share and mean mu (Q8, 256 = neutral) per expert, all images pooled

| leaf | pixels % | mu H | mu V | mu D' | mu Paeth |
|---|---|---|---|---|---|
| 0 | 47.7% | 293 | 261 | 396 | 237 |
| 1 | 13.2% | 298 | 411 | 177 | 507 |
| 2 | 11.4% | 252 | 286 | 312 | 262 |
| 3 | 7.6% | 253 | 286 | 297 | 260 |
| 4 | 3.7% | 259 | 320 | 289 | 306 |
| 5 | 0.7% | 350 | 297 | 330 | 313 |
| 6 | 10.6% | 272 | 266 | 307 | 239 |
| 7 | 4.2% | 294 | 270 | 287 | 263 |
| 8 | 1.0% | 329 | 253 | 298 | 336 |

All round-trip, determinism, state-sync, u128-reference and bounds checks passed: true.
