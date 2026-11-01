# ELLIE final three-image production benchmark

Release build, container codec 6, best of 5 encodes/decodes per cell. Raw = canonical decoded 8-bit RGB bytes of the 1024-px centre crop.

| image | mode | file B | payload B | overhead B | ratio (raw/file) | encode s | decode s | repr | roundtrip | deterministic |
|---|---|---|---|---|---|---|---|---|---|---|
| portrait_0 | untransformed | 987210 | 987161 | 49 | 3.186 | 0.315 | 0.367 | 0 | true | true |
| portrait_0 | ycocg-r | 548071 | 548022 | 49 | 5.740 | 0.367 | 0.415 | 1 | true | true |
| portrait_0 | auto | 548071 | 548022 | 49 | 5.740 | 0.781 | 0.399 | 1 | true | true |
| screen_0 | untransformed | 527130 | 527076 | 54 | 7.957 | 0.485 | 0.502 | 0 | true | true |
| screen_0 | ycocg-r | 311035 | 310981 | 54 | 13.485 | 0.379 | 0.473 | 1 | true | true |
| screen_0 | auto | 311035 | 310981 | 54 | 13.485 | 0.531 | 0.244 | 1 | true | true |
| night_0 | untransformed | 2057915 | 2057866 | 49 | 1.529 | 0.738 | 0.879 | 0 | true | true |
| night_0 | ycocg-r | 1976665 | 1976616 | 49 | 1.591 | 0.786 | 0.818 | 1 | true | true |
| night_0 | auto | 1976665 | 1976616 | 49 | 1.591 | 1.599 | 0.826 | 1 | true | true |

## Aggregate (summed)

Frozen baseline total: 3572101 B. Raw total: 10485760 B.

| mode | file B | payload B | saved vs baseline (file) | reduction | ratio (raw/file) | encode s (sum) | decode s (sum) |
|---|---|---|---|---|---|---|---|
| untransformed | 3572255 | 3572103 | -154 | -0.00% | 2.935 | 1.539 | 1.747 |
| ycocg-r | 2835771 | 2835619 | 736330 | 20.61% | 3.698 | 1.532 | 1.705 |
| auto | 2835771 | 2835619 | 736330 | 20.61% | 3.698 | 2.912 | 1.470 |

All round-trip and determinism checks passed: true.
