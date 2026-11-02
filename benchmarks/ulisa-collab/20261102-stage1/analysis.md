## Stage 1 (16 Bayer train+val images)
exact: L-O 16/16; replica==production L1 16/16
V0: L-T stream 16, orig 16; E-T planes 16, orig 16
V1: L-T stream 16, orig 16; E-T planes 16, orig 16
E-O exact 16, E-O prod16 exact 16
lattice images: ['parrot_anafi_dusk'] | secondary lattice: []
top wrap rates: sigma_fp_bike14=0.0031, leica_q_canoe=0.0025, pentax_kx_lichen=0.0011, leica_cl_chateau=0.0010

Baselines: A Lisa prod 275,889,189 B | E-O fair 265,820,463 | E-O prod16 276,546,736
L1 effect on Lisa: 286,735,435 -> 275,889,189 (-3.78%)

| arm | total B | agg % | improved | median % | worst % |
|---|---|---|---|---|---|
| L-T V0 | 277,621,679 | +0.628 | 2/16 | +0.763 | +1.767 |
| L-T V1 | 281,365,993 | +1.985 | 2/16 | +2.242 | +3.608 |
| E-T V0 | 267,718,449 | +0.714 | 2/16 | +0.825 | +1.952 |
| E-T V1 | 271,500,604 | +2.137 | 2/16 | +2.374 | +3.887 |
best variants: {'L': 'L-T V0', 'E': 'E-T V0'}

| image | split | bd | L1 | L-O B | L-T % | E-O vs L-O % | E-T vs L-T % | E-T % |
|---|---|---|---|---|---|---|---|---|
| bmpcc4k_person | train | 12 | - | 7,517,952 | -1.73 | -3.5 | -3.6 | -1.81 |
| canon_sx40_snow | train | 12 | - | 11,954,219 | +0.36 | -3.6 | -3.6 | +0.36 |
| hasselblad_tallinn | train | 16 | - | 25,974,595 | +0.63 | -2.5 | -2.4 | +0.74 |
| leica_m8_house | train | 8 | - | 5,443,197 | -6.66 | -5.8 | -6.1 | -7.00 |
| leica_q_canoe | train | 14 | - | 27,966,357 | +0.29 | -4.0 | -3.9 | +0.37 |
| lg_h815_mountain | train | 10 | - | 9,588,249 | +1.13 | -5.4 | -5.2 | +1.37 |
| parrot_anafi_dusk | train | 14 | g=16 | 12,291,116 | +1.77 | -5.8 | -5.6 | +1.95 |
| pentax_k1ii_station | train | 14 | - | 36,498,518 | +0.34 | -3.0 | -3.0 | +0.37 |
| pentax_k70_bulb | train | 14 | - | 20,396,919 | +1.76 | -3.8 | -3.6 | +1.95 |
| pixel6pro_valley | train | 14 | - | 11,299,938 | +0.03 | -3.7 | -3.7 | +0.04 |
| ricoh_gr3_dunes | train | 14 | - | 26,829,161 | +0.67 | -3.4 | -3.3 | +0.68 |
| samsung_gx20_berries | train | 12 | - | 11,666,928 | +1.12 | -3.8 | -3.7 | +1.16 |
| leica_cl_chateau | val | 14 | - | 25,248,992 | +0.85 | -3.1 | -3.1 | +0.91 |
| nexus6_courtyard | val | 10 | - | 8,006,715 | +1.38 | -5.4 | -5.2 | +1.52 |
| pentax_kx_lichen | val | 12 | - | 11,458,135 | +1.31 | -3.5 | -3.1 | +1.70 |
| sigma_fp_bike14 | val | 14 | - | 23,748,198 | +1.44 | -3.3 | -3.2 | +1.56 |

Engine advantage
- orig: ELLIE vs Lisa -3.65%, ELLIE smaller on 16/16, median -3.66%
- trans: ELLIE vs Lisa -3.57%, ELLIE smaller on 16/16, median -3.63%
- interaction I = +0.09 pp

Subsets (L-T %, E-T % vs own original)
- all (n=16): L-T +0.628%  E-T +0.714%  L-T improved 2  E-T improved 2
- excl parrot (only L1 image) (n=15): L-T +0.575%  E-T +0.658%  L-T improved 2  E-T improved 2
- excl 8-bit (n=15): L-T +0.775%  E-T +0.866%  L-T improved 1  E-T improved 1
- excl parrot+8bit+16bit (n=13): L-T +0.738%  E-T +0.824%  L-T improved 1  E-T improved 1
- parrot only (n=1): L-T +1.767%  E-T +1.952%  L-T improved 0  E-T improved 0

Timing (5 concurrent): L-T dec/L-O dec 1.097; transform+inverse = 1.29% of L-T enc

Gate (clear fail for an engine: agg > -0.25% OR <= 8/16 improved)
- L L-T V0: agg +0.628% improved 2/16 median +0.763% worst +1.767% | promising=False clear-fail=True
- E E-T V0: agg +0.714% improved 2/16 median +0.825% worst +1.952% | promising=False clear-fail=True
GATE: STOP
