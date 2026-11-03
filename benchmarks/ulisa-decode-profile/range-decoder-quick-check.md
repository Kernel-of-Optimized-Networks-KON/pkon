# Range decoder quick check (branch-free resolve + raw p=1/2 path)

Diagnostic only. Production source, container and bitstream untouched. Code: `harness/src/rc.rs`, `rcverify.rs`, `rctime.rs`. Raw output: `raw/rc/`.

## Step 1 — assembly (production `Stage4Dec::code`, inlined in `decode_tree_flank`)
Bit resolution is a real conditional branch at every inlined site (`subl bound; jb`, taken when `code < bound`). Range/code state is kept in memory (`24(%r13)`, `28(%r13)`). Refill is a loop with a `cmpq pos,len; jae` end-of-buffer check. So nothing was already branch-free.

## Correctness (both images, all six variants vs production decoder)
`sigma_fp_bike8` and `leica_m10_brick`: reconstructed samples identical, pixel CRC identical, payload bytes consumed identical, for variants A (production), B (verbatim copy), C (branch-free resolve), D (C + branch-free padded refill), E (raw p=1/2 path), F (D + raw path). Result: ALL VARIANTS EXACT (`raw/rc/verify_2img.txt`). A wider 24-file run was stopped on request (3 files had passed before it was stopped: `verify_ABORTED_partial_3files.txt`).

## Timing: one run each, A then F, pinned CPU 4, high priority, no cooldown
| image | A original | F experimental | difference |
|---|---|---|---|
| sigma_fp_bike8 (8-bit) | 0.879 s | 1.130 s | **+28.5 % (slower)** |
| leica_m10_brick (14-bit) | 10.311 s | 9.457 s | **-8.3 % (faster)** |

## Conclusion: inconclusive
The two images disagree in sign. A always ran first and F second, so order and thermal state are confounded with the variant; the earlier report measured up to 1.7x run-to-run drift on this machine, far larger than either effect. The 8-bit file mostly carries 3 decisions per sample with few raw bits, so a slowdown there is plausible (extra work per decision) but one run cannot show it. The 14-bit gain is in the direction the profile predicted, but one run does not establish it.

Not done, per instructions: no repeated runs, no per-variant split (C/D/E/F), no Pentax. To say which change helps, a few alternating runs of A/C/E/F on `leica_m10_brick` would be needed. Awaiting approval before any further runs.
