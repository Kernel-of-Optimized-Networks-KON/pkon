# ELLIE (Paeth Stage 1 + YCoCg-R) vs the established lossless benchmark (bench50 corpus, bench50 methodology)

Images: 50; valid for all five codecs: 50; invalid/failed records: 0. Crops byte-identical (sha256) to the historical bench50 manifest: 50/50.

**Raster / definitions (unchanged from `benchmarks/pkon2/20261027-155433Z-bench50/report.md`):** decoded pixels of each photo, 1024x1024 centre crop, RGB8 (JPEG sources: lossless coding of the decoded raster, not the JPEG bytes). Ratio = raw pixel bytes / stored bytes, same buffer for every codec. Sizes are the complete file/stream each codec emits: JXL bare codestream, WebP RIFF/VP8L file, PNG file, **ELLIE = production Paeth + YCoCg-R, headerless Stage 4 payload = the three per-plane streams concatenated (Y 8-bit, Co/Cg 9-bit); w/h/layout and the three stream lengths out-of-band, whereas the previous run coded the three RGB planes as one stream**; a PKON v2 container would add 24 B/image, = 1200 B total, shown below). Competitors/configs: jxl-e7-t1, jxl-e9-t1 (ffmpeg libjxl `-distance 0 -threads 1`), webp-m6-t1 (ffmpeg libwebp lossless, method 6), png-rs (Rust `png`, Best + Adaptive). Every output decoded from its own bytes and compared byte-for-byte with the canonical buffer; ELLIE additionally re-encoded and compared (determinism). Timing: best of 2 per image/codec (the established harness setting; unchanged); ELLIE and png-rs in-process, ffmpeg codecs include process spawn + temp-file I/O (`net` subtracts the per-call floor). ELLIE timing includes plane split/interleave, YCoCg-R forward/inverse and codec setup. Single-threaded ELLIE; 8-logical-CPU Windows 11 machine, release build.

## Aggregate compression (sum of stored bytes)

Raw: 158334976 B over 50 images (52428800 px).

| codec | total bytes | bpp (24 raw) | aggregate ratio | ELLIE size vs codec | mean per-image ratio | median per-image ratio |
|---|---|---|---|---|---|---|
| ellie | 43578526 | 6.650 | 3.633 | +0.0% | 4.166 | 3.641 |
| jxl-e7-t1 | 39519208 | 6.030 | 4.007 | +10.3% | 4.649 | 4.260 |
| jxl-e9-t1 | 39118943 | 5.969 | 4.048 | +11.4% | 4.718 | 4.322 |
| webp-m6-t1 | 44695086 | 6.820 | 3.543 | -2.5% | 4.125 | 3.468 |
| png-rs | 65492478 | 9.993 | 2.418 | -33.5% | 2.672 | 2.485 |

(`ELLIE size vs codec` = ELLIE bytes / codec bytes - 1; positive means ELLIE is larger. With a 24 B PKON v2 header ELLIE would be 43579726 B, ratio 3.633.)

## Timing (sum over images of best-of-2, ms)

| codec | enc total | enc net | enc MP/s net | enc MB/s net (raw) | dec total | dec net | dec MP/s net | dec MB/s net (raw) | enc net ms/img | dec net ms/img |
|---|---|---|---|---|---|---|---|---|---|---|
| ellie | 14208 | 14195 | 3.69 | 11.15 | 16313 | 16298 | 3.22 | 9.71 | 284 | 326 |
| jxl-e7-t1 | 48743 | 46831 | 1.12 | 3.38 | 22113 | 20007 | 2.62 | 7.91 | 937 | 400 |
| jxl-e9-t1 | 222487 | 220276 | 0.24 | 0.72 | 22143 | 20018 | 2.62 | 7.91 | 4406 | 400 |
| webp-m6-t1 | 545275 | 542566 | 0.10 | 0.29 | 5521 | 3433 | 15.27 | 46.12 | 10851 | 69 |
| png-rs | 33742 | 33734 | 1.55 | 4.69 | 686 | 685 | 76.54 | 231.14 | 675 | 14 |

Peak memory: the established benchmark does not measure it, so it is not measured here.

## Rankings

### Per-image size rank (1 = smallest output); count of images at each rank

| codec | rank1 | rank2 | rank3 | rank4 | rank5 | mean rank |
|---|---|---|---|---|---|---|
| ellie | 3 | 2 | 33 | 12 | 0 | 3.08 |
| jxl-e7-t1 | 2 | 42 | 1 | 5 | 0 | 2.18 |
| jxl-e9-t1 | 43 | 3 | 4 | 0 | 0 | 1.22 |
| webp-m6-t1 | 2 | 3 | 12 | 33 | 0 | 3.52 |
| png-rs | 0 | 0 | 0 | 0 | 50 | 5.00 |

### Aggregate ranks

- Compression (aggregate bytes, best first): jxl-e9-t1 > jxl-e7-t1 > ellie > webp-m6-t1 > png-rs
- Encode speed (net MP/s, fastest first): ellie > png-rs > jxl-e7-t1 > jxl-e9-t1 > webp-m6-t1
- Decode speed (net MP/s, fastest first): png-rs > webp-m6-t1 > ellie > jxl-e7-t1 > jxl-e9-t1

### Per-image encode / decode speed rank (1 = fastest, net of process floor); count of images

encode:

| codec | rank1 | rank2 | rank3 | rank4 | rank5 |
|---|---|---|---|---|---|
| ellie | 44 | 6 | 0 | 0 | 0 |
| jxl-e7-t1 | 0 | 13 | 37 | 0 | 0 |
| jxl-e9-t1 | 0 | 0 | 0 | 50 | 0 |
| webp-m6-t1 | 0 | 0 | 0 | 0 | 50 |
| png-rs | 6 | 31 | 13 | 0 | 0 |

decode:

| codec | rank1 | rank2 | rank3 | rank4 | rank5 |
|---|---|---|---|---|---|
| ellie | 0 | 0 | 41 | 6 | 3 |
| jxl-e7-t1 | 0 | 0 | 2 | 34 | 14 |
| jxl-e9-t1 | 0 | 0 | 7 | 10 | 33 |
| webp-m6-t1 | 0 | 50 | 0 | 0 | 0 |
| png-rs | 50 | 0 | 0 | 0 | 0 |

## ELLIE vs each competitor

| competitor | ELLIE agg. size vs it | per-image min / median / max | images ELLIE smaller | its net enc time / ELLIE's | its net dec time / ELLIE's |
|---|---|---|---|---|---|
| jxl-e7-t1 | +10.3% | -18.9% / +11.8% / +41.5% | 5/50 | 3.30x | 1.23x |
| jxl-e9-t1 | +11.4% | -16.5% / +13.4% / +42.9% | 5/50 | 15.52x | 1.23x |
| webp-m6-t1 | -2.5% | -12.9% / -2.6% / +16.5% | 36/50 | 38.22x | 0.21x |
| png-rs | -33.5% | -44.5% / -35.3% / -15.0% | 50/50 | 2.38x | 0.04x |

(time ratio > 1 means the competitor is slower than ELLIE; < 1 means faster.)

## Per-image results

| image | cat | raw B | ellie B | jxl-e7-t1 B | jxl-e9-t1 B | webp-m6-t1 B | png-rs B | ELLIE ratio | best | ELLIE enc/dec ms |
|---|---|---|---|---|---|---|---|---|---|---|
| outdoor_0 | outdoor | 3145728 | 922893 | 824676 | 821091 | 989404 | 1417416 | 3.409 | jxl-e9-t1 | 202/237 |
| outdoor_1 | outdoor | 3145728 | 579133 | 520781 | 510641 | 594604 | 932530 | 5.432 | jxl-e9-t1 | 421/401 |
| indoor_0 | indoor | 3145728 | 434169 | 365779 | 359179 | 447900 | 775015 | 7.245 | jxl-e9-t1 | 408/457 |
| indoor_1 | indoor | 3145728 | 700671 | 612689 | 605142 | 721894 | 1168705 | 4.490 | jxl-e9-t1 | 275/295 |
| portrait_0 | portrait | 3145728 | 500548 | 398485 | 390589 | 483336 | 880271 | 6.285 | jxl-e9-t1 | 203/243 |
| pet_0 | pet | 3145728 | 865792 | 709641 | 698480 | 911822 | 1279408 | 3.633 | jxl-e9-t1 | 209/258 |
| pet_1 | pet | 3145728 | 929175 | 770192 | 765105 | 955806 | 1332222 | 3.386 | jxl-e9-t1 | 219/267 |
| pet2_0 | pet2 | 3145728 | 946982 | 780721 | 773313 | 980572 | 1479400 | 3.322 | jxl-e9-t1 | 220/267 |
| pet2_1 | pet2 | 3145728 | 609371 | 495526 | 488803 | 603644 | 983672 | 5.162 | jxl-e9-t1 | 186/217 |
| food_0 | food | 3145728 | 1318930 | 1264540 | 1255067 | 1400906 | 1996090 | 2.385 | jxl-e9-t1 | 299/346 |
| food_1 | food | 3145728 | 892747 | 740780 | 734683 | 941478 | 1450115 | 3.524 | jxl-e9-t1 | 229/272 |
| foliage_0 | foliage | 3145728 | 1338031 | 1184936 | 1180499 | 1357336 | 2016330 | 2.351 | jxl-e9-t1 | 295/348 |
| foliage_1 | foliage | 3145728 | 773216 | 630027 | 622554 | 759766 | 1237201 | 4.068 | jxl-e9-t1 | 210/253 |
| night_0 | night | 3145728 | 1947028 | 1881297 | 1891057 | 1933568 | 2290163 | 1.616 | jxl-e7-t1 | 396/470 |
| night_1 | night | 3145728 | 728481 | 712137 | 701535 | 705974 | 1046156 | 4.318 | jxl-e9-t1 | 222/265 |
| blurry_0 | blurry | 3145728 | 1126860 | 1063542 | 1050998 | 1193420 | 1554190 | 2.792 | jxl-e9-t1 | 241/294 |
| blurry_1 | blurry | 4194304 | 721564 | 667721 | 655667 | 828564 | 1280963 | 5.813 | jxl-e9-t1 | 244/282 |
| text_0 | text | 3145728 | 659869 | 586370 | 578009 | 675746 | 1046410 | 4.767 | jxl-e9-t1 | 195/235 |
| text_1 | text | 3145728 | 1144133 | 1008865 | 998577 | 1239866 | 1716246 | 2.749 | jxl-e9-t1 | 266/322 |
| document_0 | document | 3145728 | 578906 | 533453 | 529789 | 584044 | 957594 | 5.434 | jxl-e9-t1 | 198/235 |
| document_1 | document | 3145728 | 482756 | 426214 | 413808 | 485044 | 796743 | 6.516 | jxl-e9-t1 | 198/238 |
| beach_n0 | beach | 3145728 | 610146 | 752301 | 730930 | 622632 | 959418 | 5.156 | ellie | 256/311 |
| street_n0 | street | 3145728 | 1025907 | 908644 | 902117 | 1079950 | 1522031 | 3.066 | jxl-e9-t1 | 282/333 |
| building_n0 | building | 3145728 | 850162 | 717273 | 708820 | 820972 | 1145058 | 3.700 | jxl-e9-t1 | 283/303 |
| forest_n0 | forest | 3145728 | 938096 | 764239 | 756965 | 968484 | 1495690 | 3.353 | jxl-e9-t1 | 287/333 |
| flower_n0 | flower | 3145728 | 893517 | 774581 | 767292 | 914978 | 1404606 | 3.521 | jxl-e9-t1 | 300/357 |
| sunset_n0 | sunset | 3145728 | 979885 | 913743 | 903717 | 1038204 | 1347491 | 3.210 | jxl-e9-t1 | 299/349 |
| snow_n0 | snow | 3145728 | 1315411 | 1252009 | 1248716 | 1340534 | 1776436 | 2.391 | jxl-e9-t1 | 347/397 |
| bird_n0 | bird | 3145728 | 473295 | 344952 | 334976 | 406096 | 752782 | 6.646 | jxl-e9-t1 | 221/252 |
| bridge_n0 | bridge | 3145728 | 597830 | 540970 | 530820 | 602420 | 967167 | 5.262 | jxl-e9-t1 | 219/258 |
| child_n0 | child | 3145728 | 588995 | 416191 | 412043 | 509778 | 857943 | 5.341 | jxl-e9-t1 | 290/285 |
| lake_n0 | lake | 3145728 | 951481 | 852564 | 846434 | 972500 | 1491870 | 3.306 | jxl-e9-t1 | 326/356 |
| macro_n0 | macro | 3145728 | 927018 | 824244 | 814185 | 916848 | 1210602 | 3.393 | jxl-e9-t1 | 638/633 |
| church_n0 | church | 3145728 | 862068 | 729588 | 724747 | 841722 | 1420194 | 3.649 | jxl-e9-t1 | 289/346 |
| boat_n0 | boat | 3145728 | 1129381 | 1032017 | 1028975 | 1166400 | 1580787 | 2.785 | jxl-e9-t1 | 330/350 |
| desert_n0 | desert | 3145728 | 692720 | 727834 | 721862 | 683242 | 1063023 | 4.541 | webp-m6-t1 | 226/287 |
| stairs_n0 | stairs | 3145728 | 1178793 | 1101662 | 1083329 | 1253852 | 1728009 | 2.669 | jxl-e9-t1 | 354/390 |
| woman_n0 | woman | 3145728 | 409257 | 367893 | 361013 | 393378 | 597287 | 7.686 | jxl-e9-t1 | 294/291 |
| waterfall_n0 | waterfall | 3145728 | 1124301 | 1061597 | 1055859 | 1175322 | 1518762 | 2.798 | jxl-e9-t1 | 301/378 |
| peak_n0 | peak | 3145728 | 1305012 | 1224688 | 1216205 | 1372922 | 1985072 | 2.410 | jxl-e9-t1 | 444/552 |
| restaurant_n0 | restaurant | 3145728 | 859513 | 736167 | 724298 | 902414 | 1252548 | 3.660 | jxl-e9-t1 | 283/327 |
| graffiti_n0 | graffiti | 3145728 | 973602 | 829804 | 821745 | 1018978 | 1516070 | 3.231 | jxl-e9-t1 | 307/336 |
| station_n0 | station | 3145728 | 658104 | 566806 | 557920 | 636384 | 1028440 | 4.780 | jxl-e9-t1 | 240/286 |
| clouds_n0 | clouds | 3145728 | 538084 | 570294 | 557863 | 534082 | 915485 | 5.846 | webp-m6-t1 | 211/270 |
| coast_n0 | coast | 3145728 | 498980 | 523100 | 509527 | 512628 | 797532 | 6.304 | ellie | 214/295 |
| skyline_n0 | skyline | 3145728 | 389027 | 464276 | 454777 | 403066 | 691009 | 8.086 | ellie | 190/239 |
| path_n0 | path | 3145728 | 1735996 | 1713272 | 1717184 | 1811926 | 2439490 | 1.812 | jxl-e7-t1 | 506/560 |
| river_n0 | river | 3145728 | 1101224 | 1009270 | 1002276 | 1165178 | 1600724 | 2.857 | jxl-e9-t1 | 300/340 |
| bicycle_n0 | bicycle | 3145728 | 1196949 | 1102964 | 1097034 | 1247610 | 1757880 | 2.628 | jxl-e9-t1 | 351/419 |
| lighthouse_n0 | lighthouse | 3145728 | 572517 | 517893 | 502728 | 587892 | 1032232 | 5.495 | jxl-e9-t1 | 283/275 |

## Comparison with the previous ELLIE bench50 result (RGB planes, original Stage 1)

- ELLIE total: previous 69186801 B (ratio 2.289) -> now 43578526 B (ratio 3.633); change -25608275 B (-37.01%).
- jxl-e7-t1 bytes: previous 39519208, now 39519208 (identical).
- jxl-e9-t1 bytes: previous 39118943, now 39118943 (identical).
- webp-m6-t1 bytes: previous 44695086, now 44695086 (identical).
- png-rs bytes: previous 65492478, now 65492478 (identical).
- ELLIE encode (sum best-of-2): 16590 -> 14208 ms (-14.4%); decode 20309 -> 16313 ms (-19.7%). Different runs on a non-isolated machine.
- Images where ELLIE got smaller: 50/50.

## Correctness

- Records valid & byte-exact: 250/250. Failures: none.
- ELLIE pixel-identical on 50/50 images; deterministic re-encode on all valid images (nondeterminism would invalidate the record).
- Truncation (half stream) rejected or mismatching for every codec on the calibration image (preflight, see stdout.txt).
