# ELLIE residual coding-efficiency audit

Profile Bc255S11, production Stage 1 (Paeth + Déjà Vu Shrub p=2), YCoCg-R chosen by `Auto` for all three images.
Canary: `portrait_0`, `screen_0` (RGBA, alpha plane flat), `night_0`; 1024-px centre crops; release build; 10 planes, 10,485,760 samples.
Diagnostic only. No production source was modified (the only tracked file touched is `Cargo.toml`, one `[[bin]]` entry).

**Paths**
- Harness: `benchmarks/ellie-residual-audit/exp_ellie_residual_audit.rs`
- Table generator: `benchmarks/ellie-residual-audit/summarize.py`
- Results: `benchmarks/ellie-residual-audit/20261101-audit/` — `summary.md` (all tables, per image/plane/aggregate), `metrics_long.csv` (every raw metric), `per_qs_calibration.csv`, `histograms.csv` (signed + magnitude histograms per plane, Stage 1 and Stage 2), `run.log`
- Reproduce: `cargo run --release --bin exp_ellie_residual_audit -- --out <dir>` then `python -I benchmarks/ellie-residual-audit/summarize.py <dir>` (~30 s)

## 1. Pipeline as actually implemented

| Stage | In | Out | Visible to the harness |
|---|---|---|---|
| YCoCg-R (container) | 8-bit R,G,B planes | Y (8 b), Co, Cg (9 b, offset 255) | `ycocg_r_forward_planes` |
| 1 `Stage1::encode_with_pred` | plane `u16` | signed `e = x − P`, `i32` | **e and P, directly** |
| 2 `Stage2::encode_in_place` | `e` | `r = e − B`, B = rounded running mean (shift 6, 8 frac bits) | **r, directly** |
| 3 `Stage3::code_sample` | `r` ∈ [−2M, 2M] | binary decisions `(p1 16-bit, bit)`: zero flag → unary exponent → 2 modelled mantissa bits → raw mantissa (p=½) → sign (own 9-context model) | **every `(p1, bit)`**, via a pass-through `BitIo` |
| 4 `Stage4Enc` | `(p1, bit)` | bytes; 32-bit range coder, no header, 0–4 byte termination | bytes, plus a shadow of its `range` |
| Container | plane streams | 32 B header + 18–23 B meta + payload | `pkon::parse_header` |

Stage 3's contexts (qs, qd, qe, qw, sign state) are private. The harness **re-derives them from the public r plane** with the production formulas. The probabilities, mixer and model updates are the production code. I cannot verify the replica against Stage 3's internals; the byte-identity check below does not cover it, and a wrong replica would distort only the context-conditioned comparators (Sections F and H), not the production cost.

**Integrity check:** for all three images the instrumented payload is byte-identical to the live `pkon::encode_standard_with(Auto)` payload. The harness asserts this.

## 2. Measured results (aggregate; per-image and per-plane in `summary.md`)

### Residual information (Stage 1 e → Stage 2 r)

| | night_0 | portrait_0 | screen_0 | aggregate |
|---|---|---|---|---|
| mean abs e / r | 7.79 / 7.91 | 0.530 / 0.533 | 1.23 / 1.41 | 2.99 / 3.10 |
| variance e / r | 239.6 / 240.9 | 1.56 / 1.57 | 73.8 / 72.4 | 101.9 / 101.7 |
| zero % e / r | 9.1 / 8.4 | 69.8 / 69.7 | 94.2 / 86.4 | 61.4 / 58.0 |
| **H0 signed e, b/px** | 5.187 | 1.464 | 0.646 | 2.254 |
| **H0 signed r, b/px** | 5.225 | 1.468 | 1.136 | 2.462 |
| mean signed r | +0.114 | +0.019 | +0.025 | +0.050 |

H0 is the zero-order entropy of the symbol histogram. It ignores all spatial dependence and context, so it is a baseline, not a lower bound. Cross-check: the sum of per-node static decision entropies (25.83 Mbit) equals H0(r) (25.82 Mbit) to 0.04%, so the Stage 3 decision tree itself loses nothing.

### Bit accounting (aggregate)

| quantity | bits | b/px |
|---|---|---|
| H0(e) | 23,630,574 | 2.254 |
| H0(r) | 25,816,592 | 2.462 |
| Stage 3 estimate, Σ −log2 P(actual bit) | 21,488,936 | 2.049 |
| Stage 4 coder's own interval cost | 21,488,936 | 2.049 |
| Stage 4 payload, bytes×8 | 21,488,960 | 2.049 |
| container (header+meta, 155 B) | 1,240 | — |

Payload minus model estimate is **24 bits in total** (3 bytes over 10 range-coder streams): arithmetic loss −0.007 bit, termination/alignment 24.0 bits. The container is 0.006% of the file. Per image, the payload is 1,928,443 B (night), 482,926 B (portrait) and 274,751 B (screen).

### Decision categories (production probabilities, aggregate)

| class | decisions | model bits | share of cost | bits/decision | order-0 per-node entropy | model vs order-0 |
|---|---|---|---|---|---|---|
| zero flag | 10.49 M | 3.31 M | 15.4% | 0.316 | 5.40 M | −38.7% |
| unary exponent | 11.21 M | 7.83 M | 36.4% | 0.698 | 9.50 M | −17.6% |
| modelled mantissa | 5.04 M | 4.61 M | 21.5% | 0.916 | 4.77 M | −3.2% |
| raw mantissa (p=½) | 1.77 M | 1.77 M | 8.2% | 1.000 | 1.76 M | +0.4% |
| sign | 4.40 M | 3.97 M | 18.5% | 0.901 | 4.40 M | −9.7% |

Per image, sign costs 0.987 b/decision (night), 0.823 (portrait) and 0.599 (screen). Raw plus sign is 26.7% of the cost and is close to incompressible with these features.

### Model cost against empirical comparators (aggregate bits; `summary.md` section F has every class, plane and image)

| comparator | bits | vs production |
|---|---|---|
| production Stage 3 (mix of B and C) | 21,488,936 | — |
| best single partition, honest KT (B+qw) | 22,145,890 | +3.1% |
| B(qs,qd) KT / C(qe,qw) KT | 22,329,858 / 22,540,875 | +3.9% / +4.9% |
| finest existing-feature partition B+qe+qw, **static** entropy (no learning cost, optimistic) | 21,445,939 | **−0.20%** |
| same partition, KT (learning cost included) | 22,259,960 | +3.6% |

Per image, the static finest-partition entropy relative to the production cost is −1.35% (night), +4.5% (portrait, where the model beats even the optimistic static bound) and −0.43% (screen).

### Stage 2 counterfactual (not a production configuration)

The same production Stage 3, cost only, fed e instead of r:

| | model bits on r (production) | on e (Stage 2 bypassed) | Δ |
|---|---|---|---|
| night_0 | 15,427,535 | 15,273,041 | +1.01% |
| portrait_0 | 3,863,404 | 3,849,018 | +0.37% |
| screen_0 | 2,197,997 | 1,780,182 | **+23.5%** |
| aggregate | 21,488,936 | 20,902,241 | **+2.81%** |

### Spatial structure (pooled Pearson, aggregate r)

| lag (dx,dy) | (1,0) | (2,0) | (0,1) | (0,2) | (1,1) | (−1,1) |
|---|---|---|---|---|---|---|
| signed r | 0.119 | −0.073 | 0.171 | −0.063 | 0.106 | 0.031 |
| \|r\| | 0.400 | 0.312 | 0.368 | 0.293 | 0.274 | 0.312 |

Portrait signed lag-1 correlation is 0.22 horizontally and 0.26 vertically. Magnitude correlation (~0.4) is what the qs/qe contexts model. Signed correlation is partly modelled through the sign contexts (portrait sign 0.78 b/decision), and Stage 3 has no feature that uses it for magnitudes.

### Calibration

Across qs contexts (aggregate), the decision-weighted mean |predicted P(1) − observed P(1)| is 0.72 percentage points, with a maximum of 0.9 pp. The cost of that mean miscalibration is about 16 kbit, or 0.08% of the payload. This is a weak test, because it averages over nodes within a qs bucket.

## 3. Direct answers

1. **Information in Stage 1 residuals:** zero-order 2.25 b/px (23.6 Mbit), or 2.46 b/px after Stage 2. The spread is very large by image: night 5.2 (noise, variance 240, 8% zeros), portrait 1.46, screen 0.65. These are baselines, not limits.
2. **Stage 2 effect on the distribution:** mean and variance barely move (|mean| ≤ 0.11, variance ±1%). Zero-order entropy rises 0.21 b/px overall: night +0.04, portrait +0.004, screen +0.49. On screen_0 the zero fraction falls from 94.2% to 86.4%, because the running bias leaves a nonzero B in flat regions. Under the production Stage 3, Stage 2 costs bits on all three images (+0.4% to +23.5%, +2.8% aggregate). This is a three-image measurement. It says nothing about the broader corpus, and I did not verify the basis on which Stage 2 was originally approved.
3. **Bits Stage 3 estimates:** 21,488,936 bits (2.049 b/px), 83.2% of H0(r).
4. **Bits Stage 4 produces:** 21,488,960 bits (2,686,120 B), plus 155 B of container.
5. **Gap, empirical entropy vs production cost:**
   - Stage 4 vs Stage 3 estimate: 24 bits total (about 1e-6 relative).
   - Production vs existing-feature empirical entropy: the production model is below every honest (KT) comparator by 3–5%. Against the optimistic static finest partition it is within 0.2% aggregate (night −1.35%, screen −0.43%, portrait it wins by 4.5%). That static number carries no learning cost (0.81 Mbit for that partition), so it is an upper bound on unexploited structure, not an attainable gain.
   - Against H0(r), production is 16.8% lower. That difference is context modelling already realised.
6. **Cost by category:** unary exponent 36.4%, modelled mantissa 21.5%, sign 18.5%, zero flag 15.4%, raw mantissa 8.2%. Zero and unary carry nearly all of the context-modelling gain.
7. **Residual structure the models fail to exploit:** within the existing contexts, essentially none:
   - Heterogeneity inside a B context is real (adding qe or qw cuts zero-flag KT by about 3%), but the production mixer already captures it.
   - Raw mantissa bits are biased (P(1) ≈ 0.46). Modelling them is worth at most about 7 kbit (0.03% of the total).
   - Outside the existing contexts: signed neighbour correlation of 0.1–0.27 remains in r, and Stage 3 uses it only for sign decisions. Whether more of it is extractable is not tested here, since I was told not to invent predictors.
8. **Where the opportunity lies:**
   - Stage 4 is not a source of inefficiency (24 bits).
   - Stage 3 is within about 0.2% of what its own features can deliver. That is not a small-margin claim for the whole model: it is a statement about the features it uses.
   - The evidence points at **Stage 1 / information not in Stage 3's current features**, and, for the noise-dominated image, at irreducible sensor noise (night_0: 4.9 b/px model vs 5.2 H0; the model barely beats zero-order).
   - Stage 2 is a measured loss on this canary.
   - This audit cannot split "Stage 1 predicts poorly" from "Stage 3 lacks a feature that would capture what Stage 1 leaves". Both sit upstream of the entropy coder.

## 4. Limitations

- Contexts are re-derived, not read from Stage 3 (Section 1).
- Static conditional entropies are optimistic for sparse partitions. The KT figures are the honest comparators, and I treat only them as attainable.
- The comparators use existing features only. They are not bounds on the true conditional entropy of the residual field.
- Three images, one crop size. `screen_0` is synthetic and dominates the Stage 2 loss. The aggregate mixes very different image types.
- The Stage 2 counterfactual re-uses Stage 3 as tuned for r. It was not retuned for e.
- Timing was not measured, since it is not relevant to this question.
