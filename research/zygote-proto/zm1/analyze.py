#!/usr/bin/env python3
"""ZM1 analysis: per-checkpoint, per-category tables from drive.py JSON reports.

    python3 analyze.py Z2.json N2.json [Z1.json N1.json ...]

Roles are re-derived from `comm` here (the forked app child keeps the zygote's
argv, so the driver's own role labels are not trusted), and the per-category
numbers come from the raw `smaps` dumps the driver kept under
`<out>/raw/<checkpoint>/`. Prints:

  * every checkpoint's per-category MiB for the app children and for the zygote;
  * the inherited-set CoW erosion (boot image + zygote space + non-moving space
    + class/intern/linear-alloc) from checkpoint 1 to every later checkpoint;
  * private-dirty growth per category;
  * the marginal per-session MiB when an N=1 and an N=2 report of a mode exist.
"""
import glob
import gzip
import json
import os
import re
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
import smaps as sm  # noqa: E402

CATS = ["boot_image", "zygote_space", "nonmoving_space", "region_space",
        "large_object", "class_meta", "jit", "art_runtime", "dex_apk",
        "gralloc", "malloc", "stacks", "fonts", "icu", "other_so",
        "dalvik_other", "other"]
INHERITED = ("boot_image", "zygote_space", "nonmoving_space", "class_meta")
F = ("rss", "pss", "shared_clean", "shared_dirty", "private_clean", "private_dirty")


def role(comm):
    # /proc/comm is truncated to 15 bytes (and this product's app child shows
    # up as `ixtures.counter`, i.e. the first byte is gone), so match on a
    # substring rather than a prefix.
    if "ixtures" in comm or "dev.acos" in comm or "dialog" in comm:
        return "guest"
    if comm in ("z4ns", "z4init", "z4app"):
        return "sandbox"
    if comm in ("main", "dalvikvm64"):
        return "zygote"
    return "other"


def parse_gz(path):
    lines = gzip.open(path, "rt", errors="replace").read().splitlines()
    maps, cur = [], None
    for line in lines:
        if re.match(r"^[0-9a-f]+-[0-9a-f]+ ", line):
            parts = line.split()
            name = " ".join(parts[5:]) if len(parts) > 5 else "[anon]"
            cur = {"name": name, "counts": {}}
            maps.append(cur)
            continue
        if ":" not in line or cur is None:
            continue
        k, v = line.split(":", 1)
        k = k.strip()
        if k in ("Rss", "Pss", "Shared_Clean", "Shared_Dirty", "Private_Clean",
                 "Private_Dirty", "Size"):
            try:
                cur["counts"][k] = int(v.split()[0])
            except (ValueError, IndexError):
                cur["counts"][k] = 0
    return maps


def raw_for(rawdir, cp, pid):
    short = "cp" + cp[0] if cp[0].isdigit() else "cp" + cp.split("_")[0]
    for d in (short, cp):
        hits = glob.glob(os.path.join(rawdir, d, f"*-{pid}-*.smaps.gz"))
        if hits:
            return hits[0]
    return None


def load(path):
    with open(path) as f:
        return json.load(f)


def raw_dir(path, rep):
    d = os.path.dirname(os.path.abspath(path))
    stem = os.path.basename(d)[:-5] if d.endswith(".json") else None
    cands = [os.path.join(d, "raw"),
             os.path.join(d, os.path.splitext(os.path.basename(path))[0], "raw")]
    for c in cands:
        if os.path.isdir(c):
            return c
    return cands[0]


def categories(rep, cp, pid):
    raw = raw_for(rep["_raw"], cp, pid)
    if not raw:
        return None
    return sm.aggregate(parse_gz(raw))


def sumcats(cats_list):
    out = {}
    for cats in cats_list:
        if not cats:
            continue
        for c, v in cats.items():
            a = out.setdefault(c, {k: 0.0 for k in F})
            for k in F:
                a[k] += v[k]
    return out


def mib(v):
    return round(v / 1024.0, 2)


def print_table(title, cats, pids):
    print(f"\n## {title}  (pids {pids})")
    print(f"   {'category':16s} {'rss':>8} {'pss':>8} {'sc':>8} {'sd':>8} "
          f"{'pc':>7} {'pd':>8}")
    total = {k: 0.0 for k in F}
    for c in CATS + [k for k in cats if k not in CATS]:
        v = cats.get(c)
        if not v or not any(v.get(k) for k in F):
            continue
        print(f"   {c:16s} {mib(v['rss']):8.2f} {mib(v['pss']):8.2f} "
              f"{mib(v['shared_clean']):8.2f} {mib(v['shared_dirty']):8.2f} "
              f"{mib(v['private_clean']):7.2f} {mib(v['private_dirty']):8.2f}")
        for k in F:
            total[k] += v.get(k, 0)
    print(f"   {'TOTAL':16s} {mib(total['rss']):8.2f} {mib(total['pss']):8.2f} "
          f"{mib(total['shared_clean']):8.2f} {mib(total['shared_dirty']):8.2f} "
          f"{mib(total['private_clean']):7.2f} {mib(total['private_dirty']):8.2f}")
    return total


def main():
    reps = []
    for path in sys.argv[1:]:
        rep = load(path)
        rep["_raw"] = raw_dir(path, rep)
        reps.append((f"{rep['mode']}-n{rep['n']}{'-dialog' if rep.get('dialog') else ''}",
                     rep))

    for name, rep in reps:
        print(f"\n\n========== {name} ({rep['apk']}, taps={rep.get('taps')}) ==========")
        cps = list(rep.get("checkpoints", {}))
        print("checkpoints:", ", ".join(cps))
        base_by_role = {}
        for cp in cps:
            rec = rep["checkpoints"][cp]
            by_role = {}
            for p in rec.get("processes", []):
                r = role(p["comm"])
                if r == "other":
                    continue
                by_role.setdefault(r, []).append(p["pid"])
            print(f"\n===== {cp} ({rec['at_utc']}) =====")
            print("   processes:", {r: pids for r, pids in by_role.items()})
            for r in ("guest", "zygote", "sandbox"):
                pids = by_role.get(r)
                if not pids:
                    continue
                cats = sumcats([categories(rep, cp, pid) for pid in pids])
                tot = print_table(f"{r} children aggregate", cats, pids)
                if r not in base_by_role:
                    base_by_role[r] = (cp, {c: dict(v) for c, v in cats.items()})
                if r == "guest":
                    inh = {k: mib(sum(v.get(k, 0) for c, v in cats.items() if c in INHERITED))
                           for k in ("pss", "shared_clean", "shared_dirty",
                                     "private_dirty", "rss")}
                    print(f"   inherited-set totals: {inh}")

        # CoW erosion of the inherited set for the app children
        if "guest" in base_by_role:
            bcp, bcats = base_by_role["guest"]
            print(f"\n## CoW erosion of the inherited set (guest children, vs {bcp})")
            print(f"   {'checkpoint':22s} {'inh pss':>8} {'inh SC':>8} {'inh SD':>8} "
                  f"{'inh PD':>8} {'dPD':>8} {'dSD':>8}")
            for cp in cps:
                rec = rep["checkpoints"][cp]
                pids = [p["pid"] for p in rec.get("processes", [])
                        if role(p["comm"]) == "guest"]
                if not pids:
                    continue
                cats = sumcats([categories(rep, cp, pid) for pid in pids])
                inh = {k: sum(v.get(k, 0) for c, v in cats.items() if c in INHERITED)
                       for k in F}
                b = {k: sum(v.get(k, 0) for c, v in bcats.items() if c in INHERITED)
                     for k in F}
                print(f"   {cp:22s} {mib(inh['pss']):8.2f} "
                      f"{mib(inh['shared_clean']):8.2f} {mib(inh['shared_dirty']):8.2f} "
                      f"{mib(inh['private_dirty']):8.2f} "
                      f"{mib(inh['private_dirty'] - b['private_dirty']):8.2f} "
                      f"{mib(inh['shared_dirty'] - b['shared_dirty']):8.2f}")

        # private-dirty growth per category (guest children)
        if cps:
            base = rep["checkpoints"][cps[0]]
            bpids = [p["pid"] for p in base.get("processes", [])
                     if role(p["comm"]) == "guest"]
            bcats = sumcats([categories(rep, cps[0], pid) for pid in bpids])
            print(f"\n## private_dirty growth per category (guest children, vs {cps[0]})")
            print(f"   {'category':16s}" + "".join(f"{cp[:14]:>15}" for cp in cps))
            for c in CATS:
                vals = []
                for cp in cps:
                    rec = rep["checkpoints"][cp]
                    pids = [p["pid"] for p in rec.get("processes", [])
                            if role(p["comm"]) == "guest"]
                    cats = sumcats([categories(rep, cp, pid) for pid in pids])
                    vals.append(mib(cats.get(c, {}).get("private_dirty", 0)
                                    - bcats.get(c, {}).get("private_dirty", 0)))
                if any(abs(v) > 0.05 for v in vals):
                    print(f"   {c:16s}" + "".join(f"{v:15.2f}" for v in vals))

    # marginal per session, using the reported rollup sums re-derived by role
    by_mode = {}
    for name, rep in reps:
        by_mode.setdefault(rep["mode"], {})[rep["n"]] = rep
    for mode, ns in by_mode.items():
        if 1 not in ns or 2 not in ns:
            continue
        print(f"\n\n========== marginal per session ({mode}, N=1 -> N=2) ==========")
        r1, r2 = ns[1], ns[2]
        cps = [cp for cp in r2.get("checkpoints", {}) if cp in r1.get("checkpoints", {})]
        print(f"   {'checkpoint':22s} {'N=1 Pss':>9} {'N=2 Pss':>9} {'marginal':>9} "
              f"{'N=1 PD':>9} {'N=2 PD':>9} {'marg PD':>9}")
        for cp in cps:
            vals = {}
            for n, rep in (("1", r1), ("2", r2)):
                rec = rep["checkpoints"][cp]
                pids = [p["pid"] for p in rec.get("processes", [])
                        if role(p["comm"]) in ("guest", "zygote", "sandbox")]
                vals[n] = {"pss": 0.0, "pd": 0.0}
                for pid in pids:
                    cats = categories(rep, cp, pid)
                    if not cats:
                        continue
                    vals[n]["pss"] += sum(v["pss"] for v in cats.values()) / 1024.0
                    vals[n]["pd"] += sum(v["private_dirty"] for v in cats.values()) / 1024.0
            print(f"   {cp:22s} {vals['1']['pss']:9.2f} {vals['2']['pss']:9.2f} "
                  f"{vals['2']['pss'] - vals['1']['pss']:9.2f}   "
                  f"{vals['1']['pd']:9.2f} {vals['2']['pd']:9.2f} "
                  f"{vals['2']['pd'] - vals['1']['pd']:9.2f}")


if __name__ == "__main__":
    main()
