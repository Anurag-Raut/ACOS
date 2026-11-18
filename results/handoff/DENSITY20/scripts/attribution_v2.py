"""DENSITY20b: consistency model + per-session attribution from the raw json.
python3 -I scripts/attribution_v2.py > attribution_v2.md"""
import json, glob, statistics as st, os
H = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
def load(n, arm):
    return [json.load(open(f)) for f in sorted(glob.glob(f"{H}/raw/n{n}-{arm}-r*.json"))]
def med(x): return st.median(x)
def tree(n, arm): return med([d['per_session_settled']['all_pss_mib_total'] for d in load(n, arm)])
def cls(n, arm):
    out = {}
    for d in load(n, arm):
        for a in d['attribution']['by_mapping_class']:
            r = out.setdefault(a['mapping_class'], {'pss': [], 'priv': []})
            r['pss'].append(a['pss_mib']); r['priv'].append(a['private_mib'])
    return {k: (med(v['pss']), med(v['priv'])) for k, v in out.items()}
def fit(pts):  # least squares T = a + b*N
    n = len(pts); sx = sum(p[0] for p in pts); sy = sum(p[1] for p in pts)
    sxx = sum(p[0]**2 for p in pts); sxy = sum(p[0]*p[1] for p in pts)
    b = (n*sxy - sx*sy)/(n*sxx - sx*sx); return (sy - b*sx)/n, b
print("## Linear model of settled tree PSS: T(N) = pool + slope*N (MiB)\n")
print("| arm | fit points | pool MiB | slope MiB/session | residual per N (obs-fit) |\n|---|---|---:|---:|---|")
for arm, ns in (("exec", [5, 10, 20]), ("zygote-on", [10, 20])):
    a, b = fit([(n, tree(n, arm)) for n in ns])
    res = ", ".join(f"N={n}: {tree(n,arm)-(a+b*n):+.1f}" for n in ([1, 2] + ns if arm == "exec" else ns))
    print(f"| {arm} | N={ns} | {a:.0f} | {b:.1f} | {res} |")
print("\n## Per-class private MiB per session (unique to the session, immune to foreign-sharer dilution)\n")
print("| class | exec N=20 | zygote N=20 |\n|---|---:|---:|")
e, z = cls(20, "exec"), cls(20, "zygote-on")
for k in sorted(e, key=lambda k: -e[k][1]):
    print(f"| {k} | {e[k][1]/20:.2f} | {z.get(k,(0,0))[1]/20:.2f} |")
print(f"| **sum** | {sum(v[1] for v in e.values())/20:.1f} | {sum(v[1] for v in z.values())/20:.1f} |")
