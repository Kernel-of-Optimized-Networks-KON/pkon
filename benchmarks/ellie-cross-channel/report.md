# ELLIE cross-channel residual prediction: rejected

Idea: add `alpha * eY` (Y Stage 1 residual, fixed point, alpha = k/16) to the Stage 1 Co/Cg prediction, clamp to `[0, M]`, before Stage 2.
Scored as exact Stage 4 plane stream lengths (production Bc255S11, YCoCg-R, 1024-px crops of portrait_0, screen_0, night_0; Co and Cg independent since Y is unchanged).

Plane size change vs baseline (k=0):

| k (alpha) | portrait Co / Cg | screen Co / Cg | night Co / Cg |
|---|---|---|---|
| -8 (-1/2) | +135% / +184% | +98% / +185% | +5.6% / +10.3% |
| -2 (-1/8) | +17.8% / +24.7% | +52.8% / +100% | +1.2% / +2.3% |
| -1 (-1/16) | +2.8% / +3.8% | +35.7% / +69.6% | +0.5% / +1.0% |
| +1 (+1/16) | +2.6% / +4.0% | +27.4% / +54.0% | -0.46% / -0.77% |
| +2 (+1/8) | +17.7% / +25.4% | +43.9% / +70.9% | -1.0% / -1.1% |
| +8 (+1/2) | +138% / +189% | +76.6% / +157% | -2.8% / +4.1% |

Best non-zero config (Co k=1, Cg k=1): total file bytes 2,726,990 -> ~2,772,360 (+1.66%): portrait_0 +1.2%, screen_0 +16.3%, night_0 -0.4%. Only night_0 ever gains (up to -2.8% on Co).
Conclusion: Y residual is not a usable fixed-coefficient predictor of Co/Cg residuals; any perturbation of the already tight chroma predictions costs more than the correlation recovers. Not integrated; experimental code removed. Speed was not measured (size gate failed).
