production baseline total: 2612940 B (portrait_0 481178, screen_0 222580, night_0 1909182)

| config | total B | vs prod | vs D1 | portrait | screen | night | enc ms (sum) | dec ms (sum) |
|---|---|---|---|---|---|---|---|
| prod | 2612940 | +0.000% | +0.00% | +0.00% | +0.00% | 2224 | 3008 |
| REF-BEST (leaf mixer) | 2582116 | -1.180% | -2.61% | -5.56% | -0.31% | 3008 | 2720 |
| REF-D1 (1 mixer set) | 2583403 | -1.130% | -2.50% | -5.52% | -0.27% | 2692 | 2921 |
| A:Z zigzag | 2583056 | -1.144% | -2.51% | -5.61% | -0.28% | 3224 | 3801 |
| A:K curvature | 2582507 | -1.165% | -2.53% | -5.86% | -0.27% | 3648 | 3818 |
| A:Z+K | 2582470 | -1.166% | -2.53% | -5.97% | -0.26% | 3174 | 3702 |
| B:RES local/med residual | 2582798 | -1.154% | -2.51% | -5.61% | -0.29% | 3391 | 3293 |
| B:ACT local/med activity | 2582965 | -1.147% | -2.50% | -5.63% | -0.28% | 2935 | 3306 |
| B:DIR med orientation | 2582865 | -1.151% | -2.51% | -5.80% | -0.27% | 3084 | 3446 |
| B:BRD broad level | 2582919 | -1.149% | -2.51% | -5.58% | -0.29% | 3554 | 3489 |

production timing (best of reps, ms): portrait_0: enc(auto,2 repr) 739, enc(1 repr) 396, dec 417; screen_0: enc(auto,2 repr) 958, enc(1 repr) 422, dec 513; night_0: enc(auto,2 repr) 2562, enc(1 repr) 977, dec 950; 

leaf histogram (% of pixels, config T, all planes):

| image | 0 | 1 | 2 | 3 | 4 | 5 | 6 | 7 | 8 | 9 | 10 | 11 |
|---|---|---|---|---|---|---|---|---|---|---|---|---|
| portrait_0 | 31.3 | 5.6 | 14.4 | 17.2 | 0.0 | 21.6 | 0.3 | 0.7 | 4.5 | 4.1 | 0.2 | 0.1 |
| screen_0 | 88.6 | 2.4 | 1.4 | 3.8 | 0.0 | 0.5 | 0.1 | 0.5 | 0.7 | 1.4 | 0.2 | 0.5 |
| night_0 | 0.3 | 0.3 | 0.0 | 5.3 | 0.0 | 15.6 | 0.4 | 1.8 | 35.5 | 24.4 | 10.6 | 5.8 |

all exact+deterministic: true
