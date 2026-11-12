#!/usr/bin/env python3
"""Summarise the H2 A/B runs under .cache/h2/runs into the committed evidence."""
import glob
import json
import os
import statistics
import sys

ROOT = os.path.abspath(os.path.join(os.path.dirname(__file__), "..", ".."))
RUNS = os.environ.get("ACOS_H2_RUNS", "/home/Qui/Projects/acos/.cache/h2/runs")
OUT = os.path.join(ROOT, "results", "art-runtime", "evidence", "H2")
AGES = ("0.5", "2", "5", "15")


def med(xs):
    xs = [x for x in xs if x is not None]
    return round(statistics.median(xs), 2) if xs else None


def load(pattern):
    rows = []
    for path in sorted(glob.glob(pattern)):
        try:
            with open(path) as f:
                rows.append((path, json.load(f)))
        except (OSError, json.JSONDecodeError):
            pass
    return rows


def first_action():
    out = {}
    for app in ("counter", "form-validation"):
        out[app] = {}
        for arm in ("off", "on"):
            path = os.path.join(RUNS, "first-action", f"{app}-{arm}", "result.json")
            if not os.path.exists(path):
                continue
            d = json.load(open(path))
            out[app][arm] = {
                "n": d["summary"]["all"]["n"],
                "all": d["summary"]["all"],
                "by_action": d["summary"]["by_action"],
                "ready": d["summary"]["ready"],
                "failures": d["summary"]["failures"],
                "markers": sum(1 for s in d["samples"] if s.get("early_release_marker")),
            }
    return out


def memory():
    cells = {}
    for n in (1, 5, 10):
        for arm in ("a", "ap"):
            files = sorted(glob.glob(os.path.join(RUNS, "memory", "rep*", f"{arm}-n{n}", "result.json")))
            if not files:
                continue
            per_age = {a: [] for a in AGES}
            pd = {a: [] for a in AGES}
            for path in files:
                d = json.load(open(path))
                for a in AGES:
                    c = d.get("cells", {}).get(a)
                    if c:
                        per_age[a].append(c["pss_median_mib"])
                        pd[a].append(c["private_dirty_median_mib"])
            cells[f"n{n}-{arm}"] = {
                "reps": len(files),
                "pss_median_mib": {a: med(per_age[a]) for a in AGES},
                "pss_reps": per_age,
                "private_dirty_median_mib": {a: med(pd[a]) for a in AGES},
            }
    deltas = {}
    for n in (1, 5, 10):
        a = cells.get(f"n{n}-a")
        ap = cells.get(f"n{n}-ap")
        if not a or not ap:
            continue
        deltas[str(n)] = {}
        for age in AGES:
            off = a["pss_median_mib"][age]
            on = ap["pss_median_mib"][age]
            if off:
                deltas[str(n)][age] = {
                    "off_mib": off, "on_mib": on,
                    "saving_pct": round((off - on) / off * 100.0, 1),
                    "saving_mib": round(off - on, 1),
                }
    return {"cells": cells, "deltas": deltas}


def loops():
    out = {}
    for n in (5, 10):
        out[str(n)] = {}
        for arm in ("a", "ap"):
            files = sorted(glob.glob(os.path.join(RUNS, "loop", f"n{n}", "rep*", arm, "result.json")))
            if not files:
                continue
            reps = []
            for path in files:
                d = json.load(open(path))
                reps.append({
                    "tasks": d.get("verified_tasks"),
                    "false_passes": d.get("false_passes"),
                    "tasks_per_min": d.get("tasks_per_min"),
                    "tasks_per_gib_min": (d.get("pss_sampler") or {}).get("tasks_per_gib_min"),
                    "tasks_per_gib_min_net": (d.get("sampler") or {}).get("tasks_per_gib_min_net"),
                    "pss_peak_mib": (d.get("pss_sampler") or {}).get("pss_peak_mib"),
                    "p50_ms": (d.get("task_latency_ms") or {}).get("p50"),
                    "p95_ms": (d.get("task_latency_ms") or {}).get("p95"),
                    "wall_s": d.get("wall_s"),
                    "failures": len(d.get("failures") or []),
                    "verdict": d.get("verdict"),
                })
            out[str(n)][arm] = {
                "reps": reps,
                "median": {
                    k: med([r[k] for r in reps])
                    for k in ("tasks_per_min", "tasks_per_gib_min", "tasks_per_gib_min_net",
                              "pss_peak_mib", "p50_ms", "p95_ms")
                },
            }
    deltas = {}
    for n in ("5", "10"):
        a = out.get(n, {}).get("a")
        ap = out.get(n, {}).get("ap")
        if not a or not ap:
            continue
        base = a["median"]["tasks_per_gib_min"]
        new = ap["median"]["tasks_per_gib_min"]
        deltas[n] = {
            "tasks_per_gib_min_off": base, "tasks_per_gib_min_on": new,
            "change_pct": round((new - base) / base * 100.0, 1) if base else None,
            "p50_ms_off": a["median"]["p50_ms"], "p50_ms_on": ap["median"]["p50_ms"],
            "p95_ms_off": a["median"]["p95_ms"], "p95_ms_on": ap["median"]["p95_ms"],
        }
    return {"n": out, "deltas": deltas}


def main():
    summary = {"first_action": first_action(), "memory": memory(), "loop": loops()}
    os.makedirs(OUT, exist_ok=True)
    with open(os.path.join(OUT, "summary.json"), "w") as f:
        json.dump(summary, f, indent=1, sort_keys=True)
    lines = []
    fa = summary["first_action"]
    lines.append("H2 first action after launch (ms, n per app/arm)")
    for app, arms in fa.items():
        for arm, s in arms.items():
            by = s["by_action"]
            lines.append(
                f"  {app:16s} {arm:3s} n={s['n']:2d} all p50/p95={s['all']['p50']}/{s['all']['p95']} "
                f"strict={by['tree_strict']['p50']}/{by['tree_strict']['p95']} "
                f"fast={by['tree_fast']['p50']}/{by['tree_fast']['p95']} "
                f"tap={by['tap']['p50']}/{by['tap']['p95']} ready={s['ready']['p50']} "
                f"fail={s['failures']} markers={s['markers']}")
    lines.append("")
    lines.append("H2 memory: whole-tree Pss (MiB), median of 3 interleaved reps")
    for n in (1, 5, 10):
        for arm in ("a", "ap"):
            c = summary["memory"]["cells"].get(f"n{n}-{arm}")
            if c:
                vals = " ".join(f"{age}s={c['pss_median_mib'][age]}" for age in AGES)
                lines.append(f"  N={n:<2d} {'off' if arm=='a' else 'on ':3s} {vals}")
        d = summary["memory"]["deltas"].get(str(n), {})
        if d:
            lines.append("       saving " + " ".join(
                f"{age}s={d[age]['saving_mib']}MiB/{d[age]['saving_pct']}%" for age in AGES))
    lines.append("")
    lines.append("H2 agent loop: 1 min per arm/rep, fresh launch per task")
    for n in ("5", "10"):
        for arm in ("a", "ap"):
            v = summary["loop"]["n"].get(n, {}).get(arm)
            if v:
                m = v["median"]
                lines.append(
                    f"  N={n} {'off' if arm=='a' else 'on ':3s} tasks/GiB-min pss={m['tasks_per_gib_min']} "
                    f"net={m['tasks_per_gib_min_net']} tasks/min={m['tasks_per_min']} "
                    f"p50/p95={m['p50_ms']}/{m['p95_ms']} peak={m['pss_peak_mib']} "
                    f"false={[r['false_passes'] for r in v['reps']]}")
        d = summary["loop"]["deltas"].get(n)
        if d:
            lines.append(f"       change tasks/GiB-min={d['change_pct']}% "
                         f"p50 {d['p50_ms_off']}->{d['p50_ms_on']} "
                         f"p95 {d['p95_ms_off']}->{d['p95_ms_on']}")
    text = "\n".join(lines) + "\n"
    open(os.path.join(OUT, "table.txt"), "w").write(text)
    sys.stdout.write(text)


if __name__ == "__main__":
    main()
