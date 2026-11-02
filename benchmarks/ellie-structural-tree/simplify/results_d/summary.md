production baseline total: 2612940 B (portrait_0 481178, screen_0 222580, night_0 1909182)

| config | total B | vs prod | portrait | screen | night | enc ms (sum) | dec ms (sum) |
|---|---|---|---|---|---|---|---|
| prod | 2612940 | +0.000% | +0.00% | +0.00% | +0.00% | 1807 | 1987 |
| prevbest | 2582588 | -1.162% | -2.62% | -5.65% | -0.27% | 2384 | 2406 |
| D1:noconf+1set (B,C,T) | 2583403 | -1.130% | -2.50% | -5.52% | -0.27% | 2156 | 2324 |
| D2:noconf+1set (B,T) | 2590688 | -0.852% | -2.01% | -4.87% | -0.09% | 2103 | 2049 |
| D3:noconf+1set (C,T) | 2584603 | -1.084% | -2.19% | -5.41% | -0.30% | 2001 | 2244 |
| D4:noconf+1set (T) | 2593974 | -0.726% | -1.48% | -4.61% | -0.08% | 2116 | 2184 |
| D0:noconf+1set (B,C) no tree | 2614065 | +0.043% | +0.15% | +0.10% | +0.01% | 1853 | 1841 |
| D0:noconf+1set (B) no tree | 2625248 | +0.471% | +0.92% | +1.34% | +0.26% | 1742 | 1898 |
| D0:noconf+1set (C) no tree | 2689199 | +2.919% | +4.29% | +9.27% | +1.83% | 1828 | 2616 |
| D5:noconf (B,T) leaf-mixer | 2591018 | -0.839% | -1.99% | -4.74% | -0.09% | 2368 | 2229 |
| D6:noconf+1set+limit127 (B,T) | 2592285 | -0.790% | -2.01% | -5.04% | +0.01% | 3261 | 3330 |

production timing (best of reps, ms): portrait_0: enc(auto,2 repr) 529, enc(1 repr) 236, dec 260; screen_0: enc(auto,2 repr) 2231, enc(1 repr) 681, dec 618; night_0: enc(auto,2 repr) 1338, enc(1 repr) 959, dec 864; 

leaf histogram (% of pixels, config T, all planes):

| image | 0 | 1 | 2 | 3 | 4 | 5 | 6 | 7 | 8 | 9 | 10 | 11 |
|---|---|---|---|---|---|---|---|---|---|---|---|---|
| portrait_0 | 31.3 | 5.6 | 14.4 | 17.2 | 0.0 | 21.6 | 0.3 | 0.7 | 4.5 | 4.1 | 0.2 | 0.1 |
| screen_0 | 88.6 | 2.4 | 1.4 | 3.8 | 0.0 | 0.5 | 0.1 | 0.5 | 0.7 | 1.4 | 0.2 | 0.5 |
| night_0 | 0.3 | 0.3 | 0.0 | 5.3 | 0.0 | 15.6 | 0.4 | 1.8 | 35.5 | 24.4 | 10.6 | 5.8 |

all exact+deterministic: true
