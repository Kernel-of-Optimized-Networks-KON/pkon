import json, math, statistics, sys
R = json.load(open(sys.argv[1]))['images']
split = json.load(open('benchmarks/lisa-vs-jxl-png/split.json'))
sp = {i: k for k, v in split.items() for i in v}
n = len(R)
pct = lambda a, b: 100.0 * (a - b) / b
tot = lambda f, rs=None: sum(f(r) for r in (rs or R))
LO = lambda r: r['L_O']['file_bytes']; EO = lambda r: r['E_O']['bytes']
arms = {'L-T V0': lambda r: r['L_T_V0']['bytes'], 'L-T V1': lambda r: r['L_T_V1']['bytes'],
        'E-T V0': lambda r: r['E_T_V0']['bytes'], 'E-T V1': lambda r: r['E_T_V1']['bytes']}
base = {'L': LO, 'E': EO}
P = print
P(f"## Stage 1 ({n} Bayer train+val images)")
P(f"exact: L-O {sum(r['L_O']['exact'] for r in R)}/{n}; replica==production L1 {sum(r['l1_matches_production'] for r in R)}/{n}")
for v in ('V0','V1'):
    P(f"{v}: L-T stream {sum(r['L_T_'+v]['exact_stream'] for r in R)}, orig {sum(r['L_T_'+v]['exact_original'] for r in R)}; E-T planes {sum(r['E_T_'+v]['exact_planes'] for r in R)}, orig {sum(r['E_T_'+v]['exact_original'] for r in R)}")
P(f"E-O exact {sum(r['E_O']['exact_planes'] for r in R)}, E-O prod16 exact {sum(r['E_O_prod16']['exact'] for r in R)}")
P("lattice images:", [r['id'] for r in R if r['l1']], "| secondary lattice:", [(r['id'],v) for r in R for v in ('V0','V1') if r['L_T_'+v]['secondary_l1']])
P("top wrap rates:", ", ".join(f"{r['id']}={r['L_T_V0']['wrap_rate']:.4f}" for r in sorted(R,key=lambda r:-r['L_T_V0']['wrap_rate'])[:4]))
P(f"\nBaselines: A Lisa prod {tot(LO):,} B | E-O fair {tot(EO):,} | E-O prod16 {tot(lambda r:r['E_O_prod16']['file_bytes']):,}")
P(f"L1 effect on Lisa: {tot(lambda r:r['L_O']['l1_off_file_bytes']):,} -> {tot(LO):,} ({pct(tot(LO),tot(lambda r:r['L_O']['l1_off_file_bytes'])):+.2f}%)")
P("\n| arm | total B | agg % | improved | median % | worst % |\n|---|---|---|---|---|---|")
best = {}
for name, f in arms.items():
    b = base[name[0]]; ds = [pct(f(r), b(r)) for r in R]
    P(f"| {name} | {tot(f):,} | {pct(tot(f),tot(b)):+.3f} | {sum(d<0 for d in ds)}/{n} | {statistics.median(ds):+.3f} | {max(ds):+.3f} |")
    if name[0] not in best or tot(f) < tot(arms[best[name[0]]]): best[name[0]] = name
P("best variants:", best)
LT, ET = arms[best['L']], arms[best['E']]
P("\n| image | split | bd | L1 | L-O B | L-T % | E-O vs L-O % | E-T vs L-T % | E-T % |\n|---|---|---|---|---|---|---|---|---|")
for r in sorted(R, key=lambda r: (sp[r['id']], r['id'])):
    P(f"| {r['id']} | {sp[r['id']]} | {r['bd']} | {'g=%d'%r['l1']['g'] if r['l1'] else '-'} | {LO(r):,} | {pct(LT(r),LO(r)):+.2f} | {pct(EO(r),LO(r)):+.1f} | {pct(ET(r),LT(r)):+.1f} | {pct(ET(r),EO(r)):+.2f} |")
P("\nEngine advantage")
for lab,l,e in (("orig",LO,EO),("trans",LT,ET)):
    g=[pct(e(r),l(r)) for r in R]; P(f"- {lab}: ELLIE vs Lisa {pct(tot(e),tot(l)):+.2f}%, ELLIE smaller on {sum(x<0 for x in g)}/{n}, median {statistics.median(g):+.2f}%")
I=math.log(tot(ET)/tot(LT))-math.log(tot(EO)/tot(LO)); P(f"- interaction I = {100*I:+.2f} pp")
P("\nSubsets (L-T %, E-T % vs own original)")
for lab,g in (("all",R),("excl parrot (only L1 image)",[r for r in R if not r['l1']]),("excl 8-bit",[r for r in R if r['bd']>8]),("excl parrot+8bit+16bit",[r for r in R if not r['l1'] and 8<r['bd']<16]),("parrot only",[r for r in R if r['l1']])):
    P(f"- {lab} (n={len(g)}): L-T {pct(tot(LT,g),tot(LO,g)):+.3f}%  E-T {pct(tot(ET,g),tot(EO,g)):+.3f}%  L-T improved {sum(LT(r)<LO(r) for r in g)}  E-T improved {sum(ET(r)<EO(r) for r in g)}")
v=best['L'][-2:]
P(f"\nTiming (5 concurrent): L-T dec/L-O dec {tot(lambda r:r['L_T_'+v]['dec_s'])/tot(lambda r:r['L_O']['dec_s']):.3f}; transform+inverse = {100*tot(lambda r:r['L_T_'+v]['transform_s']+r['L_T_'+v]['inverse_s'])/tot(lambda r:r['L_T_'+v]['enc_s']):.2f}% of L-T enc")
P("\nGate (clear fail for an engine: agg > -0.25% OR <= 8/16 improved)")
fails=[]
for e in 'LE':
    nm=best[e]; f=arms[nm]; b=base[e]; ds=[pct(f(r),b(r)) for r in R]; agg=pct(tot(f),tot(b)); imp=sum(d<0 for d in ds)
    cf = agg>-0.25 or imp<=8; fails.append(cf)
    P(f"- {e} {nm}: agg {agg:+.3f}% improved {imp}/{n} median {statistics.median(ds):+.3f}% worst {max(ds):+.3f}% | promising={agg<=-1 and imp>=14 and statistics.median(ds)<=-0.75 and max(ds)<=0.5} clear-fail={cf}")
P("GATE:", "STOP" if all(fails) else "PASS -> Stage 2")
