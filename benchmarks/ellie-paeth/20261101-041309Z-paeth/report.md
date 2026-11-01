# ELLIE Stage 1 Paeth experiment

Release, best of 5. Self-test: 1456 cases passed (baseline copy == production Stage 1 bit-exact; candidate round trips at bits 1-16, tiny/odd dims, adversarial patterns; full Stage 1-4 round trip at 7 depths).

Candidate: diagonal estimate D := (D + 4*Paeth(W,N,NW) + 1) >> 1 (offset-adjusted); H, V, activities, weights, clamp, fold unchanged. Representation chosen by the production auto rule (YCoCg-R only if strictly smaller file), independently per version. File size = payload + production container overhead (32 B header + ELLIE metadata); baseline file size is checked equal to the production `.pkon` length.

| image | repr base/paeth | base file B | paeth file B | delta B | delta % | base payload | paeth payload | S1 enc base/paeth ms | S1 dec base/paeth ms | enc base/paeth s | dec base/paeth s |
|---|---|---|---|---|---|---|---|---|---|---|---|
| portrait_0 | ycocg/ycocg | 548071 | 500597 | -47474 | -8.662% | 548022 | 500548 | 85.3/80.1 | 112.0/93.7 | 0.390/0.791 | 0.198/0.435 |
<!-- portrait_0: exact=true/true deterministic=true/true paeth plane bytes [313371, 104287, 82890] base [320681, 126556, 100785] -->
| screen_0 | ycocg/ycocg | 311035 | 285917 | -25118 | -8.076% | 310981 | 285863 | 107.2/90.6 | 131.1/107.5 | 0.812/0.863 | 0.353/0.488 |
<!-- screen_0: exact=true/true deterministic=true/true paeth plane bytes [163110, 74240, 48412, 101] base [178329, 81191, 51360, 101] -->
| night_0 | ycocg/ycocg | 1976665 | 1947077 | -29588 | -1.497% | 1976616 | 1947028 | 61.1/107.3 | 84.9/88.8 | 1.471/0.839 | 0.804/0.661 |
<!-- night_0: exact=true/true deterministic=true/true paeth plane bytes [649645, 699714, 597669] base [665589, 710086, 600941] -->
| **aggregate** | | 2835771 | 2733591 | -102180 | -3.603% | 2835619 | 2733439 | 253.5/278.0 | 328.1/289.9 | 2.673/2.493 | 1.355/1.584 |

Compute cost of the candidate vs baseline (sum of best-of-5): Stage 1 encode +9.7%, Stage 1 decode -11.6%, end-to-end encode -6.7%, end-to-end decode +16.9%.
All round trips pixel-exact and deterministic: true.
