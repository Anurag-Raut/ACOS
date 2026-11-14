#!/usr/bin/env python3
"""compare_observations.py -- diff two replay_scenario.py observations.

Usage:
  tools/phone/compare_observations.py --acos DIR/observations.json \
      --phone DIR/observations.json [--out report.md] [--title NAME]

Emits a per-step table an agent can read:

  step | kind | selector | ACOS (status / target / text set size) |
  phone (same) | differences | class

`class` is deliberately mechanical; PHONE-REF-1.md carries the human
classification:
  match                 same status, same target text/content-desc
  bounds                same text, different pixel bounds (density/API/inset)
  text-set              the visible text set differs
  state                 the target's enabled/checked differs
  status                one side passed and the other failed
  missing-on-phone      ACOS resolved the selector, the phone did not
  missing-on-acos       the phone resolved the selector, ACOS did not
"""

from __future__ import annotations

import argparse
import json
import sys


def load(path):
    with open(path, encoding="utf-8") as fh:
        return json.load(fh)


def tgt_text(t):
    if not t:
        return "-"
    for k in ("text", "content_desc"):
        v = (t.get(k) or "").strip()
        if v:
            return v
    return "<no text>"


def tgt_bounds(t):
    return (t or {}).get("bounds")


def fmt_bounds(b):
    if not b or len(b) != 4:
        return "-"
    return "[%d,%d][%d,%d]" % tuple(b)


def set_delta(a, b):
    a, b = set(a or []), set(b or [])
    only_a = sorted(a - b)
    only_b = sorted(b - a)
    return only_a, only_b


def classify(a, p):
    if a is None or p is None:
        return "missing-side"
    at, pt = a.get("target"), p.get("target")
    a_ok = a.get("status") in ("pass", "not_compared")
    p_ok = p.get("status") in ("pass", "not_compared", "pass_with_failure")
    if a_ok != p_ok:
        if pt is None and at is not None:
            return "missing-on-phone"
        if at is None and pt is not None:
            return "missing-on-acos"
        return "status"
    cls = "match"
    if at is not None and pt is not None:
        if tgt_text(at) != tgt_text(pt):
            return "state"
        # a platform that does not expose an attribute reports None: unknown is
        # not a difference (agent-device omits `checked` on plain Buttons)
        for key in ("enabled", "checked"):
            av, pv = at.get(key), pt.get(key)
            if av is not None and pv is not None and av != pv:
                return "state"
        ab, pb = tgt_bounds(at), tgt_bounds(pt)
        if ab and pb and list(ab) != list(pb):
            cls = "bounds"
    only_a, only_b = set_delta(a.get("visible_labels") or a.get("visible_texts"),
                               p.get("visible_labels") or p.get("visible_texts"))
    if only_a or only_b:
        if cls == "match":
            cls = "text-set"
    return cls


def main(argv=None):
    ap = argparse.ArgumentParser()
    ap.add_argument("--acos", required=True)
    ap.add_argument("--phone", required=True)
    ap.add_argument("--out")
    ap.add_argument("--title", default="")
    args = ap.parse_args(argv)

    A = load(args.acos)
    P = load(args.phone)
    a_steps = {s["index"]: s for s in A.get("steps") or []}
    p_steps = {s["index"]: s for s in P.get("steps") or []}
    rows = []
    counts = {}
    for idx in sorted(set(a_steps) | set(p_steps)):
        a, p = a_steps.get(idx), p_steps.get(idx)
        cls = classify(a, p)
        counts[cls] = counts.get(cls, 0) + 1
        ref = a or p
        only_a, only_b = set_delta(
            (a or {}).get("visible_labels") or (a or {}).get("visible_texts"),
            (p or {}).get("visible_labels") or (p or {}).get("visible_texts"))
        rows.append({
            "index": idx,
            "kind": ref.get("kind"),
            "selector": json.dumps(ref.get("selector"), sort_keys=True)
            if ref.get("selector") else "-",
            "acos": "%s / %s / %s" % (
                (a or {}).get("status", "-"), tgt_text((a or {}).get("target")),
                fmt_bounds(tgt_bounds((a or {}).get("target")))),
            "phone": "%s / %s / %s" % (
                (p or {}).get("status", "-"), tgt_text((p or {}).get("target")),
                fmt_bounds(tgt_bounds((p or {}).get("target")))),
            "text_only_ac": only_a,
            "text_only_phone": only_b,
            "class": cls,
        })

    lines = []
    title = args.title or (A.get("scenario") or {}).get("name") or "scenario"
    lines.append("### %s\n" % title)
    lines.append("| step | kind | selector | ACOS status/target/bounds | phone status/target/bounds | text only ACOS | text only phone | class |")
    lines.append("|---|---|---|---|---|---|---|---|")
    for r in rows:
        lines.append("| %d | %s | `%s` | %s | %s | %s | %s | **%s** |" % (
            r["index"], r["kind"], r["selector"][:70], r["acos"], r["phone"],
            ", ".join(r["text_only_ac"])[:90] or "-",
            ", ".join(r["text_only_phone"])[:90] or "-", r["class"]))
    lines.append("")
    lines.append("Counts: " + ", ".join("%s=%d" % (k, v) for k, v in sorted(counts.items())))
    lines.append("")
    md = "\n".join(lines)
    if args.out:
        with open(args.out, "w", encoding="utf-8") as fh:
            fh.write(md)
    print(md)
    return 0


if __name__ == "__main__":
    sys.exit(main())
