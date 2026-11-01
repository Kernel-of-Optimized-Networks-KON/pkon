# ELLIE Stage 1-4 three-image experiment

Release build, best of 3 reps. Raster = decoded 8-bit samples, centre-cropped to at most 1024x1024; per-channel planes, Stage 2/3 restart per plane (carry off) unless noted. Ratio = canonical raw bytes / encoded payload bytes (headerless; a PKON v2 container would add 24 B). Throughput is MB/s of canonical raw bytes. Overhead = Stage 4 bytes vs the ideal -log2(p) information of the Stage 3 probability stream.

| image | fmt | source | raster | file B | canonical B | ELLIE B (carry off) | ratio | ELLIE B (carry on) | ratio | enc MB/s | dec MB/s | S4 overhead % | round trip |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| portrait_0 | jpeg | 2034x2717 | 1024x1024 rgb | 742583 | 3145728 | 1005362 | 3.129 | 1005309 | 3.129 | 12.34 | 9.86 | 0.00 | exact / det |
| screen_0 | png | 2880x1628 | 1024x1024 rgba | 1865853 | 4194304 | 543314 | 7.720 | 542537 | 7.731 | 14.57 | 12.89 | 0.00 | exact / det |
| night_0 | jpeg | 2000x1329 | 1024x1024 rgb | 3600362 | 3145728 | 2069060 | 1.520 | 2069323 | 1.520 | 6.73 | 5.63 | 0.00 | exact / det |
| **aggregate** | | | | 6208798 | 10485760 | 3617736 | 2.898 | 3617169 | 2.899 | 10.38 | 8.71 | 0.0000 | all exact |

Aggregate ratios are from summed bytes. Shared `BinEnc` (5-byte flush) on the same stream totals 3617745 B vs Stage 4's 3617736 B (carry off). Determinism across repeated and fresh-codec encodes: identical bytes.

Per-image detail (overhead bytes, termination, model-only vs full timing) is in `results.json`.
