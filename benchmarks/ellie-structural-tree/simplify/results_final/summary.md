production baseline total: 2612940 B (portrait_0 481178, screen_0 222580, night_0 1909182)

| config | total B | vs prod | portrait | screen | night | enc ms (sum) | dec ms (sum) |
|---|---|---|---|---|---|---|---|
| prod | 2612940 | +0.000% | +0.00% | +0.00% | +0.00% | 1787 | 2017 |
| prevbest | 2582588 | -1.162% | -2.62% | -5.65% | -0.27% | 2317 | 2754 |
| BEST noconf (B,C,T, leaf-mixer) | 2582116 | -1.180% | -2.61% | -5.56% | -0.31% | 2170 | 2390 |
| SIMPLE D3 (C,T) noconf 1-set | 2584603 | -1.084% | -2.19% | -5.41% | -0.30% | 2155 | 2135 |
| SIMPLE D1 (B,C,T) noconf 1-set | 2583403 | -1.130% | -2.50% | -5.52% | -0.27% | 2195 | 2276 |
| NOTREE noconf 1-set (B,C) | 2614065 | +0.043% | +0.15% | +0.10% | +0.01% | 1825 | 1691 |

production timing (best of reps, ms): portrait_0: enc(auto,2 repr) 561, enc(1 repr) 410, dec 551; screen_0: enc(auto,2 repr) 993, enc(1 repr) 442, dec 612; night_0: enc(auto,2 repr) 1362, enc(1 repr) 783, dec 835; 

leaf histogram (% of pixels, config T, all planes):

| image | 0 | 1 | 2 | 3 | 4 | 5 | 6 | 7 | 8 | 9 | 10 | 11 |
|---|---|---|---|---|---|---|---|---|---|---|---|---|
| portrait_0 | 31.3 | 5.6 | 14.4 | 17.2 | 0.0 | 21.6 | 0.3 | 0.7 | 4.5 | 4.1 | 0.2 | 0.1 |
| screen_0 | 88.6 | 2.4 | 1.4 | 3.8 | 0.0 | 0.5 | 0.1 | 0.5 | 0.7 | 1.4 | 0.2 | 0.5 |
| night_0 | 0.3 | 0.3 | 0.0 | 5.3 | 0.0 | 15.6 | 0.4 | 1.8 | 35.5 | 24.4 | 10.6 | 5.8 |

all exact+deterministic: true
