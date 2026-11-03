# Range decoder controlled experiment (branch-free resolve vs raw p=1/2 path)

Supersedes the inconclusive `range-decoder-quick-check.md` (kept untouched). Raw data: `raw/rc2/` (`verify_3img.txt`, `time_<image>.txt`; every measurement is a `T rep variant seconds unix_ms` line, warm-ups are `W` lines).

## Images (3, no others)
- `leica_m10_brick` — 14-bit, 8.81 bpp, noisy/textured: primary, most raw bits per sample.
- `sigma_fp_bike8` — 8-bit, 2.54 bpp, smooth: few raw bits.
- `parrot_anafi_dusk` — 14-bit, 4.31 bpp, dark drone dusk scene: same bit depth as brick but half the bpp (lowest-bpp 14-bit Bayer in the corpus), so it separates bit depth from content.

## Variants (harness only, `harness/src/rc.rs`; production untouched)
- A: production `Stage4Dec` (baseline).
- B: branch-free resolve (mask arithmetic instead of `subl; jb`); original refill loop.
- C: dedicated `p1==32768` path (`bound = range>>1`); branchy resolve; original refill.
- D: B + C; original refill loop. (The old variant F also changed the refill, so D is a new variant, `V::G` in rc.rs.)

## Correctness (`rcab verify`, before timing)
All 4 variants x 3 images: samples identical to production, pixel CRC equal to stored, bytes consumed identical to A (26298803 / 2660993 / 11569253), no errors, no fallback. Every timed decode was also re-checked (assert) for samples + CRC.

## Method
Williams-design order (balanced for position and predecessor), rotating per repetition; 1 warm-up round of all four variants excluded; 3 s (2 s for bike8) idle before each decode; process pinned to CPU 4, High priority; one process at a time. Reps: 8 (brick, parrot), 16 (bike8). Not controlled: the laptop was not idle (Firefox, normal desktop). Absolute times are ~40% lower than the earlier session (A brick 6.3 s vs 10.3 s), so the earlier numbers were throttled; only the within-session pairing matters. Note: A is the library `Stage4Dec`, B-D are harness-crate generics; no verbatim-copy control was timed this round (see next step).

## Results (seconds; ratio = variant/A within the same repetition)
| image | var | median | min | max | CV | paired ratio median [range] | faster than A in |
|---|---|---|---|---|---|---|---|
| brick | A | 6.351 | 6.317 | 7.321 | 5.0% | 1 | - |
| | B | 5.789 | 5.716 | 6.385 | 3.7% | 0.911 [0.781, 1.011] | 7/8 |
| | C | 6.252 | 6.178 | 9.930 | 17.8% | 0.983 [0.937, 1.564] | 6/8 |
| | D | 5.762 | 5.638 | 6.240 | 3.1% | 0.905 [0.785, 0.983] | 8/8 |
| parrot | A | 3.052 | 3.040 | 3.627 | 6.1% | 1 | - |
| | B | 2.900 | 2.885 | 2.927 | 0.5% | 0.946 [0.807, 0.954] | 8/8 |
| | C | 3.037 | 3.019 | 4.853 | 18.3% | 0.996 [0.838, 1.560] | 6/8 |
| | D | 2.906 | 2.883 | 3.464 | 6.2% | 0.947 [0.829, 1.135] | 7/8 |
| bike8 | A | 0.803 | 0.799 | 0.828 | 0.9% | 1 | - |
| | B | 0.782 | 0.774 | 0.894 | 3.5% | 0.971 [0.951, 1.101] | 15/16 |
| | C | 0.801 | 0.795 | 0.830 | 1.2% | 0.997 [0.991, 1.022] | 10/16 |
| | D | 0.779 | 0.773 | 0.798 | 1.0% | 0.971 [0.953, 0.987] | 16/16 |

Min-vs-min (least disturbed): B -9.5% / -5.1% / -3.1%; C -2.2% / -0.7% / -0.5%; D -10.8% / -5.2% / -2.8% (brick / parrot / bike8). Outliers (e.g. C 9.9 s, 4.85 s) are single-run disturbances from desktop load; they are kept in the raw data and make medians the right summary.

## Analysis
- Branch-free resolve (B): faster on all three images, 3-9% (median 9.1% brick, 5.4% parrot, 2.9% bike8), with 30/32 paired wins. Largest where more decisions are near-random (high bpp), smallest on the smooth 8-bit file. Direction consistent, so the earlier "+28% slower on bike8" was drift/ordering, not a real effect.
- Raw p=1/2 path (C): no measurable effect (median -0.3% to -1.7%, min-vs-min <= 2%, within noise; one image-wise tie). Not distinguishable from zero.
- Combined (D): equals B within noise on every image (brick 0.905 vs 0.911, parrot 0.947 vs 0.946, bike8 0.971 vs 0.971). No additional benefit, no interference.

## Decision
- Branch-free range decoder: **ACCEPTED as a performance change** (consistent 3-10% decode-time gain on all 3 images, 30/32 paired wins; bit-exact). Caveat: gain measured vs the library `Stage4Dec` from a different crate; confirm by the control below before touching production.
- Raw p=1/2 fast path: **REJECTED** (no measurable gain alone or on top of B; adds a branch and code).
- Combination: no reason to prefer D over B.

## Next step (smallest)
Port only B into production `Stage4Dec::code` (3-line change, no format change) and run the existing production bench once on brick and bike8 with alternating A/B builds, plus one timed verbatim-copy control in the harness to rule out a crate/codegen offset. Do not pursue the raw path.
