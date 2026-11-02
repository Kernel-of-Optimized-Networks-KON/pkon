# Structural tree: proposed design (written before implementation)

## What ELLIE already captures
- **Shrub**: one global running mean |4x - 4*est| per H/V/D estimator (rate 1/8). No spatial or structural identity; one state for the whole plane.
- **Stage 3 (Bc255S11)**: residual-domain only. Contexts B = (qS, |aW-aN|), C = (slow E, aW). It never sees pixel values, pixel gradients or Stage 1's directional activity.
- Missing: (a) pixel-domain structure (palette/flat/edge identity), (b) whether residual size is *explained* by local pixel variation, (c) directional coherence, (d) fast-vs-slow energy change.

## 1-2. Signals (all integer, all causal, from W, N, NW, NE, the S_H/S_V/S_D sums Stage 1 already computes, and past |residuals|)
| id | signal | computation | category |
|---|---|---|---|
| F | value diversity | distinct values among {W,N,NW,NE}: 1 / 2 / >=3 | A structural identity (flat, two-level graphics edge, continuous) |
| P | predictability | g = S_H+S_V (pixel activity), R = 2aW+2aN+aNW+aNE: structured if 4R < g, failure if R >= g, else mixed | E variation source |
| D | directional coherence | min(S_H,S_V,S_D)*2 <= median | B spatial organisation |
| S | stationarity | abs(bitlen(R/6.4) - bitlen(E_slow)) >= 2 -> abrupt transition | C local stationarity |
| (smooth) | g < 16 with >=3 values | | E smooth intensity change |
Not implemented: multiscale (D) and grid/repetition detection (B): no cheap causal estimator that is not a duplicate of S/E or a pixel-history search (which is the earlier Deja Vu line).

## 3. Tree
Fixed shape, no stored thresholds: F==1 -> {R==0, R>0}; F==2 -> {R==0, R>0}; S abrupt -> TRANSITION; smooth -> SMOOTH; else P(3) x D(2) = 6 leaves. 12 leaves, id 0..11.

## 4. What it controls (measured separately)
- `T` (probability): extra mixed input in Stage 3 slot 0 (unused under Bc255S11): adaptive probability under (leaf, qS).
- `M` (mixer): mixer weight-set selected by leaf instead of qS>>3.
- `W` (predictor weighting): one Shrub state per leaf instead of one global state.
Controls: tree computed but unused (overhead); constant-leaf T context (= extra qS-only context; separates "more capacity" from "structure").

## 5. Decoder synchronisation
Leaf is a pure function of pixels/residuals strictly before the current sample, computed inside the single shared scan used by both directions. The decoder interleaves: leaf -> prediction -> decode residual with leaf context -> reconstruct pixel (Stage 1 and 3 are fused per sample, same bit order as production). No thresholds or tree are stored; metadata = 1 mode byte, counted. Tree-off mode must reproduce production payload bytes exactly (checked).
