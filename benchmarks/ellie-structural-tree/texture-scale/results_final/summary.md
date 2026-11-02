production baseline total: 2612940 B (portrait_0 481178, screen_0 222580, night_0 1909182)

| config | total B | vs prod | vs D1 | portrait | screen | night | enc ms (sum) | dec ms (sum) |
|---|---|---|---|---|---|---|---|
| prod | 2612940 | +0.000% | +0.00% | +0.00% | +0.00% | 2010 | 1614 |
| REF-BEST (leaf mixer) | 2582116 | -1.180% | -2.61% | -5.56% | -0.31% | 2469 | 2532 |
| REF-D1 (1 mixer set) | 2583403 | -1.130% | -2.50% | -5.52% | -0.27% | 1774 | 2747 |
| A:K curvature | 2582507 | -1.165% | -2.53% | -5.86% | -0.27% | 2815 | 3226 |
| A:Z+K texture | 2582470 | -1.166% | -2.53% | -5.97% | -0.26% | 2456 | 2587 |
| B:DIR med orientation | 2582865 | -1.151% | -2.51% | -5.80% | -0.27% | 2739 | 3423 |
| AB:K+DIR | 2582153 | -1.178% | -2.53% | -6.20% | -0.25% | 3032 | 3801 |
| OVH: K+DIR computed, unused | 2614065 | +0.043% | +0.15% | +0.10% | +0.01% | 2785 | 2792 |
| OVH: tree only, unused | 2614065 | +0.043% | +0.15% | +0.10% | +0.01% | 2094 | 2417 |

production timing (best of reps, ms): portrait_0: enc(auto,2 repr) 528, enc(1 repr) 389, dec 465; screen_0: enc(auto,2 repr) 1019, enc(1 repr) 440, dec 491; night_0: enc(auto,2 repr) 1534, enc(1 repr) 752, dec 1000; 

leaf histogram (% of pixels, config T, all planes):

| image | 0 | 1 | 2 | 3 | 4 | 5 | 6 | 7 | 8 | 9 | 10 | 11 |
|---|---|---|---|---|---|---|---|---|---|---|---|---|
| portrait_0 | 31.3 | 5.6 | 14.4 | 17.2 | 0.0 | 21.6 | 0.3 | 0.7 | 4.5 | 4.1 | 0.2 | 0.1 |
| screen_0 | 88.6 | 2.4 | 1.4 | 3.8 | 0.0 | 0.5 | 0.1 | 0.5 | 0.7 | 1.4 | 0.2 | 0.5 |
| night_0 | 0.3 | 0.3 | 0.0 | 5.3 | 0.0 | 15.6 | 0.4 | 1.8 | 35.5 | 24.4 | 10.6 | 5.8 |

all exact+deterministic: true
