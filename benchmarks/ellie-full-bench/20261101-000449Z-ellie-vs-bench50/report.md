# ELLIE vs the established lossless benchmark (bench50 corpus, bench50 methodology)

Images: 50; valid for all five codecs: 50; invalid/failed records: 0. Crops byte-identical (sha256) to the historical bench50 manifest: 50/50.

**Raster / definitions (unchanged from `benchmarks/pkon2/20261027-155433Z-bench50/report.md`):** decoded pixels of each photo, 1024x1024 centre crop, RGB8 (JPEG sources: lossless coding of the decoded raster, not the JPEG bytes). Ratio = raw pixel bytes / stored bytes, same buffer for every codec. Sizes are the complete file/stream each codec emits: JXL bare codestream, WebP RIFF/VP8L file, PNG file, **ELLIE = headerless Stage 4 payload** (w/h/layout out-of-band, as for earlier PKON2 reports; a PKON v2 container would add 24 B/image, = 1200 B total, shown below). Competitors/configs: jxl-e7-t1, jxl-e9-t1 (ffmpeg libjxl `-distance 0 -threads 1`), webp-m6-t1 (ffmpeg libwebp lossless, method 6), png-rs (Rust `png`, Best + Adaptive). Every output decoded from its own bytes and compared byte-for-byte with the canonical buffer; ELLIE additionally re-encoded and compared (determinism). Timing: best of 2 per image/codec; ELLIE and png-rs in-process, ffmpeg codecs include process spawn + temp-file I/O (`net` subtracts the per-call floor). ELLIE timing includes plane split/interleave and codec setup. Single-threaded ELLIE; 8-logical-CPU Windows 11 machine, release build.

## Aggregate compression (sum of stored bytes)

Raw: 158334976 B over 50 images (52428800 px).

| codec | total bytes | bpp (24 raw) | aggregate ratio | ELLIE size vs codec | mean per-image ratio | median per-image ratio |
|---|---|---|---|---|---|---|
| ellie | 69186801 | 10.557 | 2.289 | +0.0% | 2.443 | 2.239 |
| jxl-e7-t1 | 39519208 | 6.030 | 4.007 | +75.1% | 4.649 | 4.260 |
| jxl-e9-t1 | 39118943 | 5.969 | 4.048 | +76.9% | 4.718 | 4.322 |
| webp-m6-t1 | 44695086 | 6.820 | 3.543 | +54.8% | 4.125 | 3.468 |
| png-rs | 65492478 | 9.993 | 2.418 | +5.6% | 2.672 | 2.485 |

(`ELLIE size vs codec` = ELLIE bytes / codec bytes - 1; positive means ELLIE is larger. With a 24 B PKON v2 header ELLIE would be 69188001 B, ratio 2.288.)

## Timing (sum over images of best-of-2, ms)

| codec | enc total | enc net | enc MP/s net | enc MB/s net (raw) | dec total | dec net | dec MP/s net | dec MB/s net (raw) | enc net ms/img | dec net ms/img |
|---|---|---|---|---|---|---|---|---|---|---|
| ellie | 16590 | 16547 | 3.17 | 9.57 | 20309 | 20256 | 2.59 | 7.82 | 331 | 405 |
| jxl-e7-t1 | 49296 | 46393 | 1.13 | 3.41 | 21544 | 18513 | 2.83 | 8.55 | 928 | 370 |
| jxl-e9-t1 | 192495 | 189617 | 0.28 | 0.84 | 22975 | 20354 | 2.58 | 7.78 | 3792 | 407 |
| webp-m6-t1 | 444478 | 440888 | 0.12 | 0.36 | 5821 | 3165 | 16.57 | 50.03 | 8818 | 63 |
| png-rs | 26676 | 26663 | 1.97 | 5.94 | 565 | 563 | 93.04 | 280.99 | 533 | 11 |

Peak memory: the established benchmark does not measure it, so it is not measured here.

## Rankings

### Per-image size rank (1 = smallest output); count of images at each rank

| codec | rank1 | rank2 | rank3 | rank4 | rank5 | mean rank |
|---|---|---|---|---|---|---|
| ellie | 0 | 0 | 0 | 21 | 29 | 4.58 |
| jxl-e7-t1 | 2 | 42 | 6 | 0 | 0 | 2.08 |
| jxl-e9-t1 | 44 | 6 | 0 | 0 | 0 | 1.12 |
| webp-m6-t1 | 4 | 2 | 44 | 0 | 0 | 2.80 |
| png-rs | 0 | 0 | 0 | 29 | 21 | 4.42 |

### Aggregate ranks

- Compression (aggregate bytes, best first): jxl-e9-t1 > jxl-e7-t1 > webp-m6-t1 > png-rs > ellie
- Encode speed (net MP/s, fastest first): ellie > png-rs > jxl-e7-t1 > jxl-e9-t1 > webp-m6-t1
- Decode speed (net MP/s, fastest first): png-rs > webp-m6-t1 > jxl-e7-t1 > ellie > jxl-e9-t1

### Per-image encode / decode speed rank (1 = fastest, net of process floor); count of images

encode:

| codec | rank1 | rank2 | rank3 | rank4 | rank5 |
|---|---|---|---|---|---|
| ellie | 38 | 12 | 0 | 0 | 0 |
| jxl-e7-t1 | 0 | 5 | 45 | 0 | 0 |
| jxl-e9-t1 | 0 | 0 | 0 | 50 | 0 |
| webp-m6-t1 | 0 | 0 | 0 | 0 | 50 |
| png-rs | 12 | 33 | 5 | 0 | 0 |

decode:

| codec | rank1 | rank2 | rank3 | rank4 | rank5 |
|---|---|---|---|---|---|
| ellie | 0 | 0 | 18 | 11 | 21 |
| jxl-e7-t1 | 0 | 0 | 29 | 20 | 1 |
| jxl-e9-t1 | 0 | 0 | 3 | 19 | 28 |
| webp-m6-t1 | 0 | 50 | 0 | 0 | 0 |
| png-rs | 50 | 0 | 0 | 0 | 0 |

## ELLIE vs each competitor

| competitor | ELLIE agg. size vs it | per-image min / median / max | images ELLIE smaller | its net enc time / ELLIE's | its net dec time / ELLIE's |
|---|---|---|---|---|---|
| jxl-e7-t1 | +75.1% | +10.0% / +89.5% / +189.3% | 0/50 | 2.80x | 0.91x |
| jxl-e9-t1 | +76.9% | +9.4% / +91.8% / +191.3% | 0/50 | 11.46x | 1.00x |
| webp-m6-t1 | +54.8% | +7.0% / +55.5% / +164.2% | 0/50 | 26.64x | 0.16x |
| png-rs | +5.6% | -14.1% / +3.4% / +61.2% | 21/50 | 1.61x | 0.03x |

(time ratio > 1 means the competitor is slower than ELLIE; < 1 means faster.)

## Per-image results

| image | cat | raw B | ellie B | jxl-e7-t1 B | jxl-e9-t1 B | webp-m6-t1 B | png-rs B | ELLIE ratio | best | ELLIE enc/dec ms |
|---|---|---|---|---|---|---|---|---|---|---|
| outdoor_0 | outdoor | 3145728 | 1687020 | 824676 | 821091 | 989404 | 1417416 | 1.865 | jxl-e9-t1 | 840/871 |
| outdoor_1 | outdoor | 3145728 | 1046539 | 520781 | 510641 | 594604 | 932530 | 3.006 | jxl-e9-t1 | 449/432 |
| indoor_0 | indoor | 3145728 | 904861 | 365779 | 359179 | 447900 | 775015 | 3.476 | jxl-e9-t1 | 284/385 |
| indoor_1 | indoor | 3145728 | 1398528 | 612689 | 605142 | 721894 | 1168705 | 2.249 | jxl-e9-t1 | 319/419 |
| portrait_0 | portrait | 3145728 | 1005362 | 398485 | 390589 | 483336 | 880271 | 3.129 | jxl-e9-t1 | 272/486 |
| pet_0 | pet | 3145728 | 1347844 | 709641 | 698480 | 911822 | 1279408 | 2.334 | jxl-e9-t1 | 351/452 |
| pet_1 | pet | 3145728 | 1489756 | 770192 | 765105 | 955806 | 1332222 | 2.112 | jxl-e9-t1 | 410/519 |
| pet2_0 | pet2 | 3145728 | 1520676 | 780721 | 773313 | 980572 | 1479400 | 2.069 | jxl-e9-t1 | 385/517 |
| pet2_1 | pet2 | 3145728 | 1118736 | 495526 | 488803 | 603644 | 983672 | 2.812 | jxl-e9-t1 | 300/397 |
| food_0 | food | 3145728 | 1837679 | 1264540 | 1255067 | 1400906 | 1996090 | 1.712 | jxl-e9-t1 | 469/554 |
| food_1 | food | 3145728 | 1477428 | 740780 | 734683 | 941478 | 1450115 | 2.129 | jxl-e9-t1 | 330/397 |
| foliage_0 | foliage | 3145728 | 1956832 | 1184936 | 1180499 | 1357336 | 2016330 | 1.608 | jxl-e9-t1 | 422/512 |
| foliage_1 | foliage | 3145728 | 1395732 | 630027 | 622554 | 759766 | 1237201 | 2.254 | jxl-e9-t1 | 306/375 |
| night_0 | night | 3145728 | 2069060 | 1881297 | 1891057 | 1933568 | 2290163 | 1.520 | jxl-e7-t1 | 426/533 |
| night_1 | night | 3145728 | 1353098 | 712137 | 701535 | 705974 | 1046156 | 2.325 | jxl-e9-t1 | 291/359 |
| blurry_0 | blurry | 3145728 | 1440567 | 1063542 | 1050998 | 1193420 | 1554190 | 2.184 | jxl-e9-t1 | 304/378 |
| blurry_1 | blurry | 4194304 | 1165398 | 667721 | 655667 | 828564 | 1280963 | 3.599 | jxl-e9-t1 | 316/379 |
| text_0 | text | 3145728 | 1159258 | 586370 | 578009 | 675746 | 1046410 | 2.714 | jxl-e9-t1 | 273/332 |
| text_1 | text | 3145728 | 1838577 | 1008865 | 998577 | 1239866 | 1716246 | 1.711 | jxl-e9-t1 | 407/494 |
| document_0 | document | 3145728 | 1543210 | 533453 | 529789 | 584044 | 957594 | 2.038 | jxl-e9-t1 | 328/405 |
| document_1 | document | 3145728 | 1085240 | 426214 | 413808 | 485044 | 796743 | 2.899 | jxl-e9-t1 | 244/304 |
| beach_n0 | beach | 3145728 | 1511692 | 752301 | 730930 | 622632 | 959418 | 2.081 | webp-m6-t1 | 319/398 |
| street_n0 | street | 3145728 | 1503382 | 908644 | 902117 | 1079950 | 1522031 | 2.092 | jxl-e9-t1 | 311/396 |
| building_n0 | building | 3145728 | 1111133 | 717273 | 708820 | 820972 | 1145058 | 2.831 | jxl-e9-t1 | 253/309 |
| forest_n0 | forest | 3145728 | 1414453 | 764239 | 756965 | 968484 | 1495690 | 2.224 | jxl-e9-t1 | 314/390 |
| flower_n0 | flower | 3145728 | 1464069 | 774581 | 767292 | 914978 | 1404606 | 2.149 | jxl-e9-t1 | 320/397 |
| sunset_n0 | sunset | 3145728 | 1157786 | 913743 | 903717 | 1038204 | 1347491 | 2.717 | jxl-e9-t1 | 264/321 |
| snow_n0 | snow | 3145728 | 1615552 | 1252009 | 1248716 | 1340534 | 1776436 | 1.947 | jxl-e9-t1 | 351/430 |
| bird_n0 | bird | 3145728 | 782576 | 344952 | 334976 | 406096 | 752782 | 4.020 | jxl-e9-t1 | 220/264 |
| bridge_n0 | bridge | 3145728 | 1164645 | 540970 | 530820 | 602420 | 967167 | 2.701 | jxl-e9-t1 | 261/325 |
| child_n0 | child | 3145728 | 842079 | 416191 | 412043 | 509778 | 857943 | 3.736 | jxl-e9-t1 | 199/248 |
| lake_n0 | lake | 3145728 | 1501362 | 852564 | 846434 | 972500 | 1491870 | 2.095 | jxl-e9-t1 | 337/412 |
| macro_n0 | macro | 3145728 | 1085885 | 824244 | 814185 | 916848 | 1210602 | 2.897 | jxl-e9-t1 | 246/299 |
| church_n0 | church | 3145728 | 1703609 | 729588 | 724747 | 841722 | 1420194 | 1.847 | jxl-e9-t1 | 348/443 |
| boat_n0 | boat | 3145728 | 1440301 | 1032017 | 1028975 | 1166400 | 1580787 | 2.184 | jxl-e9-t1 | 362/403 |
| desert_n0 | desert | 3145728 | 1361525 | 727834 | 721862 | 683242 | 1063023 | 2.310 | webp-m6-t1 | 289/360 |
| stairs_n0 | stairs | 3145728 | 1535417 | 1101662 | 1083329 | 1253852 | 1728009 | 2.049 | jxl-e9-t1 | 350/425 |
| woman_n0 | woman | 3145728 | 608947 | 367893 | 361013 | 393378 | 597287 | 5.166 | jxl-e9-t1 | 194/232 |
| waterfall_n0 | waterfall | 3145728 | 1464823 | 1061597 | 1055859 | 1175322 | 1518762 | 2.148 | jxl-e9-t1 | 387/426 |
| peak_n0 | peak | 3145728 | 1928652 | 1224688 | 1216205 | 1372922 | 1985072 | 1.631 | jxl-e9-t1 | 422/514 |
| restaurant_n0 | restaurant | 3145728 | 1159772 | 736167 | 724298 | 902414 | 1252548 | 2.712 | jxl-e9-t1 | 271/324 |
| graffiti_n0 | graffiti | 3145728 | 1476474 | 829804 | 821745 | 1018978 | 1516070 | 2.131 | jxl-e9-t1 | 313/388 |
| station_n0 | station | 3145728 | 1335636 | 566806 | 557920 | 636384 | 1028440 | 2.355 | jxl-e9-t1 | 286/354 |
| clouds_n0 | clouds | 3145728 | 1158039 | 570294 | 557863 | 534082 | 915485 | 2.716 | webp-m6-t1 | 258/320 |
| coast_n0 | coast | 3145728 | 1276303 | 523100 | 509527 | 512628 | 797532 | 2.465 | jxl-e9-t1 | 291/356 |
| skyline_n0 | skyline | 3145728 | 956650 | 464276 | 454777 | 403066 | 691009 | 3.288 | webp-m6-t1 | 218/271 |
| path_n0 | path | 3145728 | 2389843 | 1713272 | 1717184 | 1811926 | 2439490 | 1.316 | jxl-e7-t1 | 487/600 |
| river_n0 | river | 3145728 | 1411524 | 1009270 | 1002276 | 1165178 | 1600724 | 2.229 | jxl-e9-t1 | 325/386 |
| bicycle_n0 | bicycle | 3145728 | 1748274 | 1102964 | 1097034 | 1247610 | 1757880 | 1.799 | jxl-e9-t1 | 376/470 |
| lighthouse_n0 | lighthouse | 3145728 | 1234997 | 517893 | 502728 | 587892 | 1032232 | 2.547 | jxl-e9-t1 | 293/351 |

## ELLIE-only measurements (not part of any cross-codec comparison)

- Modelled cost: ideal -log2(p) Stage 3 information 69186785 B vs Stage 4 actual 69186801 B -> Stage 4 overhead 0.0000%.
- carry_model=true (Stage 3 state continues across planes): 69198131 B vs 69186801 B (+0.016%). Cross-codec table uses the default (carry off).
- Model-only (Stages 1-3, no coder) encode time: 13670 ms total vs full encode 16590 ms.

## Correctness

- Records valid & byte-exact: 250/250. Failures: none.
- ELLIE pixel-identical on 50/50 images; deterministic re-encode on all valid images (nondeterminism would invalidate the record).
- Truncation (half stream) rejected or mismatching for every codec on the calibration image (preflight, see stdout.txt).
