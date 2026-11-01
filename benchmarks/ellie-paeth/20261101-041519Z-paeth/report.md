# ELLIE Stage 1 Paeth experiment

Release, best of 5. Self-test: 1456 cases passed (baseline copy == production Stage 1 bit-exact; candidate round trips at bits 1-16, tiny/odd dims, adversarial patterns; full Stage 1-4 round trip at 7 depths).

Candidate: diagonal estimate D := (D + 4*Paeth(W,N,NW) + 1) >> 1 (offset-adjusted); H, V, activities, weights, clamp, fold unchanged. Representation chosen by the production auto rule (YCoCg-R only if strictly smaller file), independently per version. File size = payload + production container overhead (32 B header + ELLIE metadata); baseline file size is checked equal to the production `.pkon` length.

| image | repr base/paeth | base file B | paeth file B | delta B | delta % | base payload | paeth payload | S1 enc base/paeth ms | S1 dec base/paeth ms | enc base/paeth s | dec base/paeth s |
|---|---|---|---|---|---|---|---|---|---|---|---|
| portrait_0 | ycocg/ycocg | 548071 | 500597 | -47474 | -8.662% | 548022 | 500548 | 36.2/42.2 | 49.1/54.3 | 0.383/0.370 | 0.189/0.187 |
<!-- portrait_0: exact=true/true deterministic=true/true paeth plane bytes [313371, 104287, 82890] base [320681, 126556, 100785] -->
| screen_0 | ycocg/ycocg | 311035 | 285917 | -25118 | -8.076% | 310981 | 285863 | 66.3/68.4 | 74.3/76.6 | 0.451/0.567 | 0.224/0.287 |
<!-- screen_0: exact=true/true deterministic=true/true paeth plane bytes [163110, 74240, 48412, 101] base [178329, 81191, 51360, 101] -->
| night_0 | ycocg/ycocg | 1976665 | 1947077 | -29588 | -1.497% | 1976616 | 1947028 | 49.3/91.2 | 74.9/104.2 | 1.045/1.390 | 0.686/0.930 |
<!-- night_0: exact=true/true deterministic=true/true paeth plane bytes [649645, 699714, 597669] base [665589, 710086, 600941] -->
| **aggregate** | | 2835771 | 2733591 | -102180 | -3.603% | 2835619 | 2733439 | 151.9/201.8 | 198.3/235.0 | 1.880/2.326 | 1.098/1.404 |

Compute cost of the candidate vs baseline (sum of best-of-5): Stage 1 encode +32.8%, Stage 1 decode +18.5%, end-to-end encode +23.8%, end-to-end decode +27.9%.
All round trips pixel-exact and deterministic: true.
