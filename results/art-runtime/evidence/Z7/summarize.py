#!/usr/bin/env python3
"""Z7 tables from the raw harness JSONs (median over interleaved repetitions)."""
import glob
import json
import os
import statistics

RUNS = os.environ.get("ACOS_Z7_RUNS", "/home/Qui/Projects/acos/.cache/z7-runs")
ARMS = ("a", "ap", "c", "cp")
AGES = ("0.5", "2", "5", "15", "60")


def lifetime_rows():
    rows = {}
    for n in (5, 10):
        for arm in ARMS:
            files = sorted(glob.glob(f"{RUNS}/lifetime/rep*-n{n}/{arm}.json"))
            cells = {}
            for age in AGES:
                vals, pds, ages = [], [], []
                for f in files:
                    d = json.load(open(f))
                    c = (d.get("cells") or {}).get(age)
                    if not c:
                        continue
                    vals.append(c["pss_median_mib"])
                    pds.append(c["private_dirty_median_mib"])
                    ages.append((c["child_age_min_s"], c["child_age_max_s"]))
                if not vals:
                    continue
                cells[age] = {
                    "reps": len(vals),
                    "pss_median_mib": round(statistics.median(vals), 1),
                    "pss_range_mib": [round(min(vals), 1), round(max(vals), 1)],
                    "pss_per_rep": vals,
                    "private_dirty_median_mib": round(statistics.median(pds), 1),
                    "child_age_range_s": [min(a[0] for a in ages),
                                          max(a[1] for a in ages)],
                }
            rows[(n, arm)] = {"files": [os.path.basename(f) for f in files],
                              "cells": cells}
    return rows


def savings(rows):
    out = {}
    for n in (5, 10):
        out[n] = {}
        for age in AGES:
            base_a = rows[(n, "a")]["cells"].get(age)
            base_ap = rows[(n, "ap")]["cells"].get(age)
            if not base_a or not base_ap:
                continue
            row = {}
            for arm in ARMS:
                c = rows[(n, arm)]["cells"].get(age)
                if not c:
                    continue
                row[arm] = {
                    "vs_a_pct": round((base_a["pss_median_mib"] - c["pss_median_mib"])
                                      / base_a["pss_median_mib"] * 100, 1),
                    "vs_ap_pct": round((base_ap["pss_median_mib"] - c["pss_median_mib"])
                                       / base_ap["pss_median_mib"] * 100, 1),
                }
            out[n][age] = row
    return out


def loop_rows():
    out = {}
    for f in sorted(glob.glob(f"{RUNS}/loop/n10-*.json")):
        arm = os.path.basename(f)[4:-5]
        d = json.load(open(f))
        pss = d.get("pss_sampler") or {}
        net = d.get("sampler") or {}
        out[arm] = {
            "verdict": d.get("verdict"),
            "wall_s": d.get("wall_s"),
            "verified_tasks": d.get("verified_tasks"),
            "rounds": len(d.get("rounds") or []),
            "tasks_per_min": round((d.get("verified_tasks") or 0)
                                   / ((d.get("wall_s") or 1) / 60.0), 2),
            "pss_gib_min": pss.get("gib_min"),
            "tasks_per_gib_min_pss": pss.get("tasks_per_gib_min"),
            "pss_peak_mib": pss.get("pss_peak_mib"),
            "net_anon_shmem_gib_min": net.get("net_anon_shmem_gib_min"),
            "tasks_per_gib_min_net": net.get("tasks_per_gib_min_net"),
            "job_cgroup_memory_current_gib_min": net.get("gib_min"),
            "latency_ms": d.get("task_latency_ms"),
            "exits_clean_rounds": sum(1 for r in d.get("rounds") or []
                                      if r.get("exits_clean")),
            # The loop runs checked the guest-namespace pid written in
            # sessions/zygote.pid, so that field is not a host liveness fact.
            # The authoritative teardown result is cycles_n5 below.
            "zygote_alive_after_close": "NOT_TRACKED_IN_LOOP",
            "failures": d.get("failures"),
        }
    return out


def cycle_rows():
    out = {}
    for f in sorted(glob.glob(f"{RUNS}/cycles/n5-*.json")):
        arm = os.path.basename(f)[3:-5]
        d = json.load(open(f))
        zs = [r for r in d.get("rounds") or [] if r.get("zygote_before")]
        out[arm] = {
            "verdict": d.get("verdict"),
            "rounds": len(d.get("rounds") or []),
            "verified_tasks": d.get("verified_tasks"),
            "wall_s": d.get("wall_s"),
            "tasks_per_gib_min_pss": (d.get("pss_sampler") or {}).get("tasks_per_gib_min"),
            "exits_clean_rounds": sum(1 for r in d.get("rounds") or []
                                      if r.get("exits_clean")),
            "zygote_alive_after_close_rounds": sum(
                1 for r in zs if r.get("zygote_alive_after_close")),
            "zygote_fds": ([r["zygote_before"]["fds"] for r in zs] if zs else None),
            "zygote_threads": ([r["zygote_before"]["threads"] for r in zs] if zs else None),
            "zygote_rss_kib_range": ([min(r["zygote_before"]["rss_kib"] for r in zs),
                                      max(r["zygote_before"]["rss_kib"] for r in zs)]
                                     if zs else None),
        }
    return out


def form_rows():
    out = {}
    for f in sorted(glob.glob(f"{RUNS}/form/n*-*.json")):
        base = os.path.basename(f)[:-5]
        n, arm = base[1:].split("-", 1)
        d = json.load(open(f))
        tasks = [s.get("task", {}) for s in (d.get("sessions") or {}).values()]
        out[base] = {
            "n": int(n), "arm": arm, "verdict": d.get("verdict"),
            "verified": sum(1 for t in tasks if t.get("verified")),
            "sessions": len(tasks),
            "failures": d.get("failures"),
        }
    return out


def main():
    rows = lifetime_rows()
    sav = savings(rows)
    loops = loop_rows()
    cycles = cycle_rows()
    forms = form_rows()
    doc = {"lifetime": {f"n{n}-{arm}": rows[(n, arm)] for n in (5, 10) for arm in ARMS},
           "savings": sav, "loop_n10": loops, "cycles_n5": cycles, "form": forms}
    here = os.path.dirname(os.path.abspath(__file__))
    json.dump(doc, open(os.path.join(here, "summary.json"), "w"), indent=1, sort_keys=True)
    lines = []
    lines.append("LIFETIME whole-system SumPss (MiB, median of reps; [min,max])")
    lines.append("n arm age0.5 age2 age5 age15 age60")
    for n in (5, 10):
        for arm in ARMS:
            cs = rows[(n, arm)]["cells"]
            vals = " ".join(f"{cs[a]['pss_median_mib']:.1f}[{cs[a]['pss_range_mib'][0]:.1f},{cs[a]['pss_range_mib'][1]:.1f}]"
                            if a in cs else "-" for a in AGES)
            lines.append(f"{n} {arm:2s} {vals}")
    lines.append("")
    lines.append("SAVINGS % (positive = less memory than the baseline)")
    for n in (5, 10):
        for age in AGES:
            row = sav.get(n, {}).get(age, {})
            if not row:
                continue
            parts = [f"{arm}: vs_a={row[arm]['vs_a_pct']:+.1f} vs_ap={row[arm]['vs_ap_pct']:+.1f}"
                     for arm in ARMS if arm in row]
            lines.append(f"n{n} t{age}: " + " | ".join(parts))
    lines.append("")
    lines.append("LOOP N=10 (P5-style round: fresh launch per task)")
    for arm in ARMS:
        r = loops.get(arm)
        if not r:
            continue
        lat = r.get("latency_ms") or {}
        lines.append(
            f"{arm:2s} tasks={r['verified_tasks']} wall={r['wall_s']}s rounds={r['rounds']} "
            f"tasks/min={r['tasks_per_min']} tasks/GiB-min(PSS)={r['tasks_per_gib_min_pss']} "
            f"PSS-GiB-min={r['pss_gib_min']} peak={r['pss_peak_mib']}MiB "
            f"p50={lat.get('p50')}ms p95={lat.get('p95')}ms "
            f"net={r['tasks_per_gib_min_net']} exits_clean={r['exits_clean_rounds']}/{r['rounds']} "
            f"zygote_liveness=see_cycles_n5")
    lines.append("")
    lines.append("CYCLES N=5 (20 fixed launch->verify->reset rounds per arm)")
    for arm in ARMS:
        r = cycles.get(arm)
        if not r:
            continue
        lines.append(
            f"{arm:2s} rounds={r['rounds']} tasks={r['verified_tasks']} wall={r['wall_s']}s "
            f"tasks/GiB-min(PSS)={r['tasks_per_gib_min_pss']} "
            f"exits_clean={r['exits_clean_rounds']}/{r['rounds']} "
            f"zygote_alive_after_close={r['zygote_alive_after_close_rounds']} "
            f"zygote_fds={r['zygote_fds']} threads={r['zygote_threads']} "
            f"rss_kib_range={r['zygote_rss_kib_range']}")
    lines.append("")
    lines.append("FORM-VALIDATION lifetime/correctness")
    for name, r in forms.items():
        lines.append(f"{name}: n={r['n']} arm={r['arm']} verdict={r['verdict']} "
                     f"verified={r['verified']}/{r['sessions']} failures={len(r['failures'] or [])}")
    text = "\n".join(lines) + "\n"
    open(os.path.join(here, "table.txt"), "w").write(text)
    print(text)


if __name__ == "__main__":
    main()
