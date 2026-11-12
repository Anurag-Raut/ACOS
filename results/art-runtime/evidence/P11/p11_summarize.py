#!/usr/bin/env python3
"""Render the P11 markdown tables from the harness JSONs."""
import json
import sys
from collections import Counter
from pathlib import Path


def mode_json(root, name, mode):
    p = Path(root) / ("%s-%s.json" % (name, mode))
    return json.load(open(p)) if p.exists() else None


def bound_counts(case):
    return Counter(str(r.get("bound")) for r in case["rows"])


def latency(case):
    lat = case["summary"].get("latency") or {}
    return lat.get("p50"), lat.get("p95")


def main():
    root = sys.argv[1]
    out = []
    docs = {m: mode_json(root, "settle", m) for m in ("quiet", "event")}
    names = [c["case"]["name"] for c in (docs.get("quiet") or docs.get("event"))["cases"]]
    out.append("| case | quiet p50/p95 ms | quiet bound | quiet false pass | quiet early render | event p50/p95 ms | event bound | event false pass | event early render |")
    out.append("|---|---:|---|---:|---:|---:|---|---:|---:|")
    for name in names:
        cells = [name]
        for mode in ("quiet", "event"):
            doc = docs.get(mode)
            case = next(c for c in doc["cases"] if c["case"]["name"] == name) if doc else None
            if not case:
                cells += ["—", "—", "—", "—"]
                continue
            p50, p95 = latency(case)
            bounds = ", ".join("%s:%d" % (k, v) for k, v in sorted(bound_counts(case).items()))
            s = case["summary"]
            if case["case"].get("expect_timeout"):
                cells += ["%s/%s" % (p50, p95), bounds,
                          "n/a (timeout)", "n/a"]
            elif case["case"].get("long"):
                cells += ["%s/%s" % (p50, p95), bounds,
                          "n/a (long delay)", "n/a"]
            else:
                cells += ["%s/%s" % (p50, p95), bounds,
                          str(s.get("false_passes")), str(s.get("early_renders"))]
        out.append("| " + " | ".join(cells) + " |")
    out.append("")
    out.append("| case | quiet notes | event notes |")
    out.append("|---|---|---|")
    for name in names:
        notes = []
        for mode in ("quiet", "event"):
            doc = docs.get(mode)
            case = next(c for c in doc["cases"] if c["case"]["name"] == name) if doc else None
            if not case:
                notes.append("—"); continue
            s = case["summary"]
            if case["case"].get("expect_timeout"):
                notes.append("timeouts %s/%s, never settled %s/%s" % (
                    s.get("timeouts"), s.get("n"), s.get("not_settled"), s.get("n")))
            elif case["case"].get("long"):
                notes.append("early replies %s/%s, snapshot before final %s/%s, final reached %s/%s" % (
                    s.get("early_replies"), s.get("n"), s.get("snapshot_before_final"),
                    s.get("n"), s.get("final_reached"), s.get("n")))
            else:
                notes.append("tree_ok %s/%s, png_ok %s/%s" % (
                    s.get("tree_ok"), s.get("n"), s.get("png_ok"), s.get("n")))
        out.append("| %s | %s | %s |" % (name, notes[0], notes[1]))
    out.append("")
    out.append("| counter M8 | p50 ms | p95 ms | ok/n | bounds |")
    out.append("|---|---:|---:|---:|---|")
    for mode in ("quiet", "event"):
        doc = mode_json(root, "counter", mode)
        if not doc:
            out.append("| %s | — | — | — | — |" % mode); continue
        rows = doc["rows"]
        s = doc["summary"]
        bounds = Counter(str(r.get("bound")) for r in rows)
        out.append("| %s | %s | %s | %s/%s | %s |" % (
            mode, s.get("p50_ms"), s.get("p95_ms"), s.get("ok"), s.get("n"),
            ", ".join("%s:%d" % (k, v) for k, v in sorted(bounds.items()))))
    print("\n".join(out))


if __name__ == "__main__":
    main()
