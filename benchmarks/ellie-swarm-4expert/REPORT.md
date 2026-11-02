# ELLIE four-expert swarm: report (canary only, release, actual stream sizes)

Source: `exp_ellie_swarm4.rs`. Data: `run-canary/summary.md`, `run-canary/per_image.csv`, `run.log`. Production: 2,612,940 B (portrait 481,178 / screen 222,580 / night 1,909,182).

| config | total B | vs prod | portrait | screen | night |
|---|---|---|---|---|---|
| prod-copy (gate, byte-identical) | 2,612,940 | 0 | 0 | 0 | 0 |
| P4-prodrel (4 experts, production Shrub, no tree) | 2,633,408 | +0.783% | +1.65% | -0.86% | +0.76% |
| P4-shared (shared reliability, no tree) | 2,642,665 | +1.138% | +2.38% | +0.42% | +0.91% |
| P4-shared-tree5 | 2,620,383 | +0.285% | -2.27% | -0.55% | +1.03% |
| P4-shared-tree9 | 2,621,571 | +0.330% | -2.28% | -0.56% | +1.09% |
| P4-shared-Lpx (ablation: Paeth L at pixel scale) | 2,625,595 | +0.484% | -0.28% | -0.62% | +0.81% |

Result: negative. No swarm configuration beats production in aggregate. Nothing promoted.
