# ULISA collaboration viability: frozen criteria (Stages 1-2, train+val only)

Source: owner authorization "Final Protocol" (thresholds section 8) + the design report. Frozen before any measurement.

## Scope
- Corpus: frozen 25-image corpus (`benchmarks/lisa-vs-jxl-png/split.json`). Bayer only (24); X-Trans `x100s_xtrans_chart` excluded (N/A, not a loss).
- Stages 1-2 use TRAIN (12 Bayer) + VAL (4 Bayer) = 16 images. The 8 test images are never loaded (harness refuses without `--final`).
- Held-out thresholds "7 of 8" are scaled to train+val as "14 of 16" (87.5%). This is a stated adaptation, not a relaxation.

## Representation
- Original: mosaic after production L1 normalization (coding depth d). Transformed: modular green difference on the NORMALIZED samples,
  `c = (v - Ghat + H) mod 2^d`, `H = 2^(d-1)`, d = normalized coding depth; G sites unchanged.
- V0: `Ghat = (N+S+W+E+2)>>2`. V1: direction by |W-E| vs |N-S| (smoother pair, `(a+b+1)>>1`), V0 on ties. Borders: index reflection.
- Variant selection: whichever of V0/V1 has fewer total train+val bytes for the engine in question. No further tuning.
- Wide (d+1) variant: diagnostic only, not run unless Stage 1 is promising.

## Engines / arms
L-O: production `pkon::encode_raw` stream. L-T: Lisa stage APIs (`bayer::encode_stage2_with_mode` -> `classify_stage3` -> `encode_stage4`) on the derived mosaic at depth d, no secondary normalization
(derived-mosaic range/GCD measured; secondary-L1 bytes recorded separately only if derived GCD > 1).
E-O/E-T: four image-parity planes through `EllieCodec` (production default profile Bc255S11Tree) at bits = d (fair arm); E-O also measured via production container at 16 bits (no L1) as reference.
Bytes: Lisa = container-equivalent (32 header + meta + 5 if L1 + payload; +1 transform id byte for L-T). ELLIE = sum of plane payloads + 40 B container-equivalent per plane (+1 byte transform id for E-T).

## Representation promising (per engine, vs its own original arm, train+val)
aggregate bytes <= -1.0%; >= 14/16 images improved; median per-image <= -0.75%; no image worse than +0.5%; (Stage 2) beats best control by >= 0.5 pp.
## Stage 1 clear failure => STOP (no Stage 2, no test set)
For BOTH engines (best variant): aggregate > -0.25%  OR  <= 8/16 images improved.
Otherwise (not a clear fail) Stage 2 runs.
## Engine advantage
Meaningful: whole-image gap >= 1.0% of whole-image bytes, same direction on >= 14/16 images. Also report the interaction I = ln(E-T/L-T) - ln(E-O/L-O).
## Collaboration
Clear pass: Gamma_fixed >= 1.5% vs best fixed independent strategy; positive on >= 14/16 (7/8 on held-out); >= 1.0% with a fixed component-to-engine rule chosen on train+val;
leave-one-out Gamma >= 1.0%; exact reconstruction everywhere. Fail: Gamma < 0.5% or positive on <= 8/16 (<=4/8). Else inconclusive.
S_composite = S_G + S_R' + S_B' + metadata; metadata = per-component lengths (4 B each x 3 streams) + engine id + representation id + CFA phase (4 B) + 2 x range-coder termination (8 B) + fixed container header 32 B => charged 64 B/image beyond the engines' own container-equivalent bytes.
Baselines: A production Lisa stream; B best FIXED independent strategy (one engine/representation assignment fixed on train+val); C per-image oracle (diagnostic only, labelled).
## Cost limits
Decode <= 1.25x production Lisa; peak memory <= 1.25x; transform <= 5% of encode time.
