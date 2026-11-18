#!/usr/bin/env python3
"""Aggregate the DENSITY20 cohort JSONs into the handoff tables.

Reads every `raw/n<N>-<arm>-r<R>.json` and writes:
  * summary.json  -- per-cohort rows, medians, marginal per-session memory
  * tables.md     -- the three tables the README embeds
No number here is estimated: every cell is either read from a cohort JSON or a
documented median over the reps of one cohort.

Usage: python3 aggregate.py [--raw DIR] [--out DIR]
"""

from __future__ import annotations

import argparse
import json
import statistics
import sys
from pathlib import Path

WT = Path(__file__).resolve().parents[4]
HERE = Path(__file__).resolve().parents[1]


def load(raw: Path):
    cohorts = {}
    for path in sorted(raw.glob("n*-r*.json")):
        doc = json.loads(path.read_text())
        # skip cohorts that never ran (zygote build missing, etc.)
        key = (doc["n"], doc["arm"])
        cohorts.setdefault(key, []).append((path.name, doc))
    return cohorts


def med(values):
    values = [v for v in values if v is not None]
    return round(statistics.median(values), 3) if values else None


def rng(values):
    values = [v for v in values if v is not None]
    if not values:
        return None
    return [round(min(values), 3), round(max(values), 3)]


ROWS = ["1-exec", "2-exec", "5-exec", "10-exec", "10-zygote-on",
        "20-exec", "20-zygote-on"]


def cohort_row(key, docs):
    n, arm = key
    ok = [d for _, d in docs if d.get("verified_tasks")]
    bad = [name for name, d in docs if not d.get("verified_tasks")]
    def col(fn):
        return [fn(d) for d in ok]
    per_pss = col(lambda d: d["per_session_settled"]["all_pss_mib_per_session"]
                  if d.get("per_session_settled") else None)
    per_guest = col(lambda d: d["per_session_settled"]["guest_pss_mib_per_session"]
                    if d.get("per_session_settled") else None)
    per_priv = col(lambda d: d["per_session_settled"]["guest_private_mib_per_session"]
                   if d.get("per_session_settled") else None)
    per_host = col(lambda d: d["per_session_settled"]["host_pss_mib_per_session"]
                   if d.get("per_session_settled") else None)
    return {
        "cohort": "%s-%s" % (n, arm),
        "n": n,
        "arm": arm,
        "reps_ok": len(ok),
        "reps_failed": bad,
        "verified_tasks": col(lambda d: d["verified_tasks"]),
        "measured_tasks": col(lambda d: d["measured_tasks"]),
        "failures": sum(len(d.get("failed_tasks") or []) for d in ok),
        "wall_s_median": med(col(lambda d: d.get("wall_s"))),
        "gib_min_pss_median": med(col(lambda d: d.get("gib_min_pss"))),
        "tasks_per_gib_min_median": med(col(lambda d: d.get("verified_per_gib_min_pss"))),
        "tasks_per_gib_min_range": rng(col(lambda d: d.get("verified_per_gib_min_pss"))),
        "per_session_pss_mib_median": med(per_pss),
        "per_session_pss_mib_range": rng(per_pss),
        "per_session_guest_pss_mib_median": med(per_guest),
        "per_session_guest_private_mib_median": med(per_priv),
        "per_session_host_pss_mib_median": med(per_host),
        "tree_pss_at_settle_mib_median": med(col(lambda d: d["per_session_settled"]["all_pss_mib_total"]
                                                 if d.get("per_session_settled") else None)),
        "peak_tree_pss_mib_median": med(col(lambda d: d.get("peak_tree_pss_mib"))),
        "host_used_pct_peak_median": med(col(lambda d: d.get("host_used_pct_peak"))),
        "cpu_pct_one_core_median": med(col(lambda d: d.get("cpu_pct_one_core"))),
        "cpu_core_s_median": med(col(lambda d: d.get("cpu_core_seconds"))),
        "gib_min_uss_median": med(col(lambda d: d.get("gib_min_uss"))),
    }


def marginal(rows):
    """Marginal per-session memory between adjacent N (settled tree PSS)."""
    by_n = {r["n"]: r for r in rows if r["arm"] == "exec"}
    out = []
    ns = sorted(by_n)
    for lo, hi in zip(ns, ns[1:]):
        a, b = by_n[lo], by_n[hi]
        if a["tree_pss_at_settle_mib_median"] and b["tree_pss_at_settle_mib_median"]:
            out.append({
                "from_n": lo, "to_n": hi,
                "marginal_mib_per_session": round(
                    (b["tree_pss_at_settle_mib_median"] - a["tree_pss_at_settle_mib_median"]) /
                    (hi - lo), 2),
            })
    return out


def attribution(key, docs):
    """Top mapping classes from the settle snapshot, per session, median rep."""
    n, arm = key
    per_class = {}
    sessions = []
    for name, d in docs:
        attr = d.get("attribution")
        if not attr:
            continue
        sessions.append(n)
        for row in attr["by_mapping_class"]:
            per_class.setdefault(row["mapping_class"], []).append(row)
    rows = []
    for label, samples in per_class.items():
        pss = [s["pss_mib"] for s in samples]
        priv = [s["private_mib"] for s in samples]
        shar = [s["shared_mib"] for s in samples]
        rows.append({
            "mapping_class": label,
            "total_pss_mib": med(pss),
            "per_session_pss_mib": round(med(pss) / n, 2) if med(pss) else None,
            "private_mib": med(priv),
            "shared_mib": med(shar),
            "pct_of_tree_pss": med([s["pct_of_tree_pss"] for s in samples]),
        })
    rows.sort(key=lambda r: -(r["total_pss_mib"] or 0))
    return rows


def fmt(v, nd=1):
    if v is None:
        return "—"
    if isinstance(v, list):
        return "–".join(fmt(x, nd) for x in v)
    if isinstance(v, float):
        return f"{v:,.{nd}f}"
    return str(v)


def tables(rows, marg, attrs):
    out = []
    out.append("### D1. Cohort results (median of the reps that verified every task)\n")
    out.append("| N | arm | reps | verified tasks | failures | wall s | GiB·min (PSS) | tasks/GiB·min | range |")
    out.append("|---:|---|---:|---|---:|---:|---:|---:|---|")
    for r in rows:
        out.append("| %d | %s | %d | %s | %d | %s | %s | **%s** | %s |" % (
            r["n"], r["arm"], r["reps_ok"],
            "%d/%d" % (sum(r["verified_tasks"]) // max(1, r["reps_ok"]), r["measured_tasks"][0]) if r["measured_tasks"] else "—",
            r["failures"], fmt(r["wall_s_median"], 2), fmt(r["gib_min_pss_median"], 4),
            fmt(r["tasks_per_gib_min_median"]), fmt(r["tasks_per_gib_min_range"])))
    out.append("")
    out.append("### D2. Memory per session (settled state: every session holding a live guest)\n")
    out.append("| N | arm | per-session PSS MiB | range | guest PSS | guest USS (private) | host-side (serve) | tree PSS total MiB | peak tree PSS MiB | host used % peak | CPU % of one core |")
    out.append("|---:|---|---:|---|---:|---:|---:|---:|---:|---:|---:|")
    for r in rows:
        out.append("| %d | %s | **%s** | %s | %s | %s | %s | %s | %s | %s | %s |" % (
            r["n"], r["arm"], fmt(r["per_session_pss_mib_median"]),
            fmt(r["per_session_pss_mib_range"]),
            fmt(r["per_session_guest_pss_mib_median"]),
            fmt(r["per_session_guest_private_mib_median"]),
            fmt(r["per_session_host_pss_mib_median"]),
            fmt(r["tree_pss_at_settle_mib_median"]),
            fmt(r["peak_tree_pss_mib_median"]),
            fmt(r["host_used_pct_peak_median"]),
            fmt(r["cpu_pct_one_core_median"], 0)))
    out.append("")
    out.append("### D3. Marginal per-session memory (exec arm, settled tree PSS)\n")
    out.append("| from N | to N | MiB per additional session |")
    out.append("|---:|---:|---:|")
    for m in marg:
        out.append("| %d | %d | **%s** |" % (m["from_n"], m["to_n"], fmt(m["marginal_mib_per_session"])))
    out.append("")
    out.append("### D4. Where the memory goes (settled, per session, median rep)\n")
    for key in ("1-exec", "10-exec", "20-exec", "10-zygote-on", "20-zygote-on"):
        if key not in attrs:
            continue
        n = attrs[key]["n"]
        out.append("**%s** (%d sessions)\n" % (attrs[key]["label"], n))
        out.append("| # | mapping class | total PSS MiB | per session MiB | private MiB | shared MiB | % of tree PSS |")
        out.append("|---:|---|---:|---:|---:|---:|---:|")
        for i, r in enumerate(attrs[key]["rows"][:12], 1):
            out.append("| %d | `%s` | %s | %s | %s | %s | %s |" % (
                i, r["mapping_class"], fmt(r["total_pss_mib"]), fmt(r["per_session_pss_mib"]),
                fmt(r["private_mib"]), fmt(r["shared_mib"]), fmt(r["pct_of_tree_pss"], 2)))
        out.append("")
    return "\n".join(out) + "\n"


def main(argv=None):
    ap = argparse.ArgumentParser()
    ap.add_argument("--raw", default=str(HERE / "raw"))
    ap.add_argument("--out", default=str(HERE))
    args = ap.parse_args(argv)
    raw = Path(args.raw)
    cohorts = load(raw)
    rows = [cohort_row(k, v) for k, v in sorted(cohorts.items(),
                                                key=lambda kv: (kv[0][0], kv[0][1]))]
    marg = marginal(rows)
    attrs = {}
    labels = {"1-exec": "N=1 exec", "10-exec": "N=10 exec",
              "20-exec": "N=20 exec", "10-zygote-on": "N=10 zygote (warm-fork)",
              "20-zygote-on": "N=20 zygote (warm-fork)"}
    for key in cohorts:
        label = "%s-%s" % key
        if label in labels:
            attrs[label] = {"n": key[0], "label": labels[label],
                            "rows": attribution(key, cohorts[key])}
    doc = {"schema": "acos-density20-summary/1",
           "cohorts": rows, "marginal": marg,
           "attribution": {k: v["rows"] for k, v in attrs.items()},
           "host": {"cpu_count": __import__("os").cpu_count()}}
    (Path(args.out) / "summary.json").write_text(json.dumps(doc, indent=2) + "\n")
    (Path(args.out) / "tables.md").write_text(tables(rows, marg, attrs))
    print(tables(rows, marg, attrs))
    return 0


if __name__ == "__main__":
    sys.exit(main())
