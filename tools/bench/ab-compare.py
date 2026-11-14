#!/usr/bin/env python3
"""ab-compare.py -- lattice-aware A/B comparison of two sets of bench baselines.

PERF-REL3. The old compare (`acos_bench.py --compare`) gates on the median of
per-rep p50s. For the fast-settle metrics (M4 launch fast, M6 reset fast, M8 tap
fast) a p50 is whichever frame slot holds the middle sample, so that gate flips
by a whole frame between reps (results/bench/REL3-AB.md). This tool prints the
old gate *and* the lattice view, and gives a verdict on numbers that do not
flip.

  tools/bench/ab-compare.py --a results/bench/run-main/*.json \\
                            --b results/bench/run-rel3/*.json

Sections:
  1. old gate    -- median of per-rep p50s (unchanged rule: per-metric threshold
                    or the category floor), printed for every metric;
  2. trimmed     -- pooled 5 %-trimmed mean per arm (the proposal's central
                    value), with delta % and delta ms;
  3. slots       -- share of samples per frame slot on ONE shared lattice
                    (period measured from the union of both arms, origin = the
                    fastest sample of the union), plus the first-slot share;
  4. verdict     -- fast metrics: REGRESSION when the pooled trimmed mean is
                    worse by > +5 % AND > +2 ms; improved when better by the
                    same margin; `slot-shift` is flagged separately when the
                    first-slot share moves >= 10 percentage points. Other
                    metrics keep the old gate's verdict.

PROPOSAL, not an adopted gate: the orchestrator decides (see
results/handoff/PERF-REL3.md). The old gate output is always printed, so this
tool never hides it. Read-only: it writes nothing unless --json is given.

Old baseline JSONs (no `samples` field) still load: the trimmed/slot sections
then say "no raw samples" and only the old gate is scored.
"""

from __future__ import annotations

import argparse
import json
import statistics
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))

import bench_lattice as L  # noqa: E402

# Mirror of acos_bench.METRICS order/labels for the metrics this tool needs to
# label well; any metric found in the docs is still compared (label = key).
LABELS = {
    "m4_launch_fast_ms": ("M4", "M4 warm launch (fast)"),
    "m6_reset_fast_ms": ("M6", "M6 reset -> first correct screen (fast)"),
    "m8_tap_counter_fast_ms": ("M8", "M8 tap -> settled, counter (fast)"),
    "m8_tap_form_fast_ms": ("M8", "M8 tap -> settled, form (fast)"),
    "m4_launch_strict_ms": ("M4", "M4 warm launch (strict)"),
    "m6_reset_strict_ms": ("M6", "M6 reset -> first correct screen (strict)"),
    "m8_tap_counter_strict_ms": ("M8", "M8 tap -> settled, counter (strict)"),
    "m8_tap_form_strict_ms": ("M8", "M8 tap -> settled, form (strict)"),
    "m5_screenshot_ms": ("M5", "M5 fresh screenshot (PNG)"),
    "m7_tree_strict_ms": ("M7", "M7 UI tree with text (strict)"),
    "m7_tree_fast_ms": ("M7", "M7 UI tree with text (fast)"),
    "teardown_ms": ("GL1", "acos_destroy teardown_ms"),
    "tap_ack_ms": ("tap ack", "tap acknowledgement"),
    "m3_cold_start_ms": ("M3", "M3 cold start -> first correct screen"),
}

FAST = ("m4_launch_fast_ms", "m6_reset_fast_ms",
        "m8_tap_counter_fast_ms", "m8_tap_form_fast_ms")

ORDER = ["m3_cold_start_ms", "m4_launch_strict_ms", "m4_launch_fast_ms",
         "m5_screenshot_ms", "m6_reset_strict_ms", "m6_reset_fast_ms",
         "m7_tree_strict_ms", "m7_tree_fast_ms", "m8_tap_counter_strict_ms",
         "m8_tap_counter_fast_ms", "m8_tap_form_strict_ms",
         "m8_tap_form_fast_ms", "tap_ack_ms", "teardown_ms"]


def load_arm(paths):
    docs = []
    for path in paths:
        try:
            docs.append(json.loads(Path(path).read_text()))
        except (OSError, ValueError) as exc:
            raise SystemExit("ab-compare: cannot read %s: %s" % (path, exc))
    return docs


def arm_metrics(docs):
    """key -> {'rep_p50': [...], 'samples': [...], 'category': str, ...}."""
    out = {}
    for doc in docs:
        for key, block in (doc.get("metrics") or {}).items():
            row = out.setdefault(key, {
                "rep_p50": [], "samples": [], "docs": 0,
                "category": block.get("category") or "latency",
                "unit": block.get("unit") or "ms",
                "label": (LABELS.get(key) or (block.get("metric"),
                                              block.get("label") or key))[1],
                "threshold_pct": None, "threshold_abs": None,
                "samples_from": None,
            })
            row["docs"] += 1
            row["rep_p50"].extend(
                v for v in (block.get("rep_p50") or []) if v is not None)
            raw = block.get("samples")
            source = "samples"
            if not raw:
                raw = (block.get("lattice") or {}).get("samples")
                source = "lattice.samples"
            if raw:
                row["samples"].extend(float(v) for v in raw if v is not None)
                row["samples_from"] = source
            if block.get("threshold_pct") is not None:
                row["threshold_pct"] = block["threshold_pct"]
                row["threshold_abs"] = block.get("threshold_abs")
    return out


def old_gate(a, b, key):
    """The existing rule, unchanged: medians of per-rep p50s against thresholds."""
    av = [v for v in a.get("rep_p50") or [] if v is not None]
    bv = [v for v in b.get("rep_p50") or [] if v is not None]
    if not av or not bv:
        return {"verdict": "missing", "median_a": None, "median_b": None}
    ma, mb = statistics.median(av), statistics.median(bv)
    delta = mb - ma
    pct = 100.0 * delta / ma if ma else None
    cat = b.get("category") or a.get("category") or "latency"
    thr_pct = b.get("threshold_pct")
    if thr_pct is None:
        thr_pct = L.MIN_PCT.get(cat, 5.0)
    thr_abs = b.get("threshold_abs")
    if thr_abs is None:
        thr_abs = L.ABS_FLOOR.get(cat, 2.0)
    beyond = abs(delta) > max(thr_abs, thr_pct / 100.0 * abs(ma))
    if cat == "count":
        beyond = mb > ma
    verdict = "within-noise"
    if beyond and delta > 0:
        verdict = "REGRESSION"
    elif beyond and delta < 0:
        verdict = "improved"
    return {"verdict": verdict, "median_a": ma, "median_b": mb,
            "delta": delta, "delta_pct": pct, "threshold_pct": thr_pct,
            "threshold_abs": thr_abs, "n_a": len(av), "n_b": len(bv)}


def lattice_gate(a, b, key, gate_pct=L.LATTICE_GATE_PCT,
                 gate_abs=L.LATTICE_GATE_ABS_MS, slot_shift=L.SLOT_SHIFT_PP,
                 trim=0.05):
    """Score one metric on pooled trimmed means + slot shares (one lattice)."""
    av, bv = a.get("samples") or [], b.get("samples") or []
    row = {"verdict": "no-samples", "n_a": len(av), "n_b": len(bv),
           "trim_a": None, "trim_b": None, "delta": None, "delta_pct": None,
           "frame_period_ms": None, "frame_period_source": None,
           "slot_shares_a": {}, "slot_shares_b": {}, "slot_delta_pp": {},
           "first_slot_share_a": None, "first_slot_share_b": None,
           "first_slot_delta_pp": None, "slot_shift": False,
           "max_slot_delta_pp": None}
    if not av or not bv:
        return row
    union = list(av) + list(bv)
    measured = L.estimate_frame_period(union)
    period = measured or L.DEFAULT_FRAME_PERIOD_MS
    origin = min(union)
    row["frame_period_ms"] = round(period, 3)
    row["frame_period_source"] = "measured" if measured else "fallback"
    det_a = L.slot_detail(av, period=period, origin=origin)
    det_b = L.slot_detail(bv, period=period, origin=origin)
    row["slot_shares_a"] = {str(r["slot"]): r["share"] for r in det_a["slots"]}
    row["slot_shares_b"] = {str(r["slot"]): r["share"] for r in det_b["slots"]}
    slots = sorted({int(k) for k in row["slot_shares_a"]}
                   | {int(k) for k in row["slot_shares_b"]})
    for k in slots:
        pct_a = 100.0 * row["slot_shares_a"].get(str(k), 0.0)
        pct_b = 100.0 * row["slot_shares_b"].get(str(k), 0.0)
        row["slot_delta_pp"][str(k)] = round(pct_b - pct_a, 1)
    row["first_slot_share_a"] = det_a["first_slot_share"]
    row["first_slot_share_b"] = det_b["first_slot_share"]
    if (row["first_slot_share_a"] is not None
            and row["first_slot_share_b"] is not None):
        fsa = 100.0 * row["first_slot_share_a"]
        fsb = 100.0 * row["first_slot_share_b"]
        row["first_slot_delta_pp"] = round(fsb - fsa, 1)
        row["slot_shift"] = abs(fsb - fsa) >= slot_shift
    if row["slot_delta_pp"]:
        row["max_slot_delta_pp"] = max(abs(v) for v in
                                       row["slot_delta_pp"].values())
    row["trim_a"] = L.trimmed_mean(av, trim)
    row["trim_b"] = L.trimmed_mean(bv, trim)
    delta = row["trim_b"] - row["trim_a"]
    pct = 100.0 * delta / row["trim_a"] if row["trim_a"] else None
    row["delta"] = round(delta, 3)
    row["delta_pct"] = round(pct, 2) if pct is not None else None
    worse = delta > gate_abs and pct is not None and pct > gate_pct
    better = -delta > gate_abs and pct is not None and -pct > gate_pct
    row["verdict"] = ("REGRESSION" if worse else
                      "improved" if better else "within-noise")
    return row


def fmt(value, digits=2):
    if value is None:
        return "-"
    if isinstance(value, float):
        return ("%." + str(digits) + "f") % value
    return str(value)


def shares_text(shares):
    if not shares:
        return "-"
    return " ".join("%s:%.0f%%" % (k, 100.0 * v)
                    for k, v in sorted(shares.items(), key=lambda kv: int(kv[0])))


def pct_delta_text(value):
    if value is None:
        return "-"
    return "%+.1f%%" % value


def main(argv=None):
    ap = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--a", nargs="+", required=True, metavar="JSON",
                    help="arm A: one or more merged baseline JSONs")
    ap.add_argument("--b", nargs="+", required=True, metavar="JSON",
                    help="arm B: one or more merged baseline JSONs")
    ap.add_argument("--label-a", default="A")
    ap.add_argument("--label-b", default="B")
    ap.add_argument("--gate-pct", type=float, default=L.LATTICE_GATE_PCT,
                    help="lattice gate: pooled trimmed-mean percent (default %g)"
                         % L.LATTICE_GATE_PCT)
    ap.add_argument("--gate-abs-ms", type=float, default=L.LATTICE_GATE_ABS_MS,
                    help="lattice gate: absolute ms floor (default %g)"
                         % L.LATTICE_GATE_ABS_MS)
    ap.add_argument("--slot-shift-pp", type=float, default=L.SLOT_SHIFT_PP,
                    help="flag a first-slot share shift >= this many points "
                         "(default %g)" % L.SLOT_SHIFT_PP)
    ap.add_argument("--trim", type=float, default=0.05,
                    help="trimmed-mean fraction per side (default 0.05)")
    ap.add_argument("--metrics", default="",
                    help="comma-separated metric keys to restrict to (default: all)")
    ap.add_argument("--json", default="", help="also write the rows as JSON here")
    ap.add_argument("--quiet", action="store_true", help="only the verdict lines")
    args = ap.parse_args(argv)

    docs_a, docs_b = load_arm(args.a), load_arm(args.b)
    ma, mb = arm_metrics(docs_a), arm_metrics(docs_b)
    only = [k.strip() for k in args.metrics.split(",") if k.strip()]
    keys = [k for k in ORDER if k in ma or k in mb]
    keys += sorted(k for k in set(ma) | set(mb) if k not in keys)
    if only:
        keys = [k for k in keys if k in only]

    rows = []
    for key in keys:
        a = ma.get(key) or {"rep_p50": [], "samples": [], "category": "latency"}
        b = mb.get(key) or {"rep_p50": [], "samples": [], "category": "latency"}
        row = {"id": key, "label": (LABELS.get(key) or
                                    (None, key))[1],
               "fast": key in FAST, "old": old_gate(a, b, key)}
        if key in FAST:
            row["lattice"] = lattice_gate(
                a, b, key, gate_pct=args.gate_pct, gate_abs=args.gate_abs_ms,
                slot_shift=args.slot_shift_pp, trim=args.trim)
        else:
            row["lattice"] = None
        rows.append(row)

    if not args.quiet:
        print("# %s vs %s -- lattice-aware A/B (PERF-REL3)\n" %
              (args.label_a, args.label_b))
        print("Arms: %s = %d file(s) [%s]; %s = %d file(s) [%s]\n" % (
            args.label_a, len(docs_a),
            ", ".join((d.get("commit") or "")[:8] or d.get("utc") or "?" for d in docs_a),
            args.label_b, len(docs_b),
            ", ".join((d.get("commit") or "")[:8] or d.get("utc") or "?" for d in docs_b)))

        print("## 1. Old gate -- medians of per-rep p50s (unchanged rule)\n")
        print("| metric | %s | %s | delta | delta %% | threshold | verdict |" %
              (args.label_a, args.label_b))
        print("|---|---:|---:|---:|---:|---:|---|")
        for row in rows:
            old = row["old"]
            thr = ("±%s%% / %s" % (fmt(old.get("threshold_pct")),
                                  fmt(old.get("threshold_abs")))
                   if old.get("threshold_pct") is not None else "-")
            print("| %s | %s | %s | %s | %s | %s | %s |" % (
                row["label"], fmt(old.get("median_a")), fmt(old.get("median_b")),
                fmt(old.get("delta")), pct_delta_text(old.get("delta_pct")),
                thr, old["verdict"]))
        print()

        print("## 2. Pooled 5 %-trimmed mean, and 3. frame-slot shares\n")
        print("| metric | n %s/%s | %s trimmed | %s trimmed | delta | delta %% | "
              "frame ms | first slot %s/%s | slot shares %s | slot shares %s | "
              "verdict |" % (
                  args.label_a, args.label_b, args.label_a, args.label_b,
                  args.label_a, args.label_b, args.label_a, args.label_b))
        print("|---|---:|---:|---:|---:|---:|---:|---:|---|---|---|---|")
        for row in rows:
            lat = row["lattice"]
            if lat is None:
                continue
            print("| %s | %d/%d | %s | %s | %+.2f | %s | %s | %s/%s | %s | %s | "
                  "%s%s |" % (
                      row["label"], lat["n_a"], lat["n_b"], fmt(lat["trim_a"]),
                      fmt(lat["trim_b"]), lat["delta"] or 0.0,
                      pct_delta_text(lat["delta_pct"]),
                      fmt(lat["frame_period_ms"]),
                      ("%.0f%%" % (100 * lat["first_slot_share_a"])
                       if lat["first_slot_share_a"] is not None else "-"),
                      ("%.0f%%" % (100 * lat["first_slot_share_b"])
                       if lat["first_slot_share_b"] is not None else "-"),
                      shares_text(lat["slot_shares_a"]),
                      shares_text(lat["slot_shares_b"]),
                      lat["verdict"],
                      " (slot-shift %+.0f pp)" % lat["first_slot_delta_pp"]
                      if lat["slot_shift"] else ""))
        print()

    fast_rows = [r for r in rows if r["fast"]]
    old_regressions = [r for r in rows if r["old"]["verdict"] == "REGRESSION"]
    lat_regressions = [r for r in fast_rows
                       if (r["lattice"] or {}).get("verdict") == "REGRESSION"]
    slot_shifts = [r for r in fast_rows if (r["lattice"] or {}).get("slot_shift")]
    no_samples = [r for r in fast_rows
                  if (r["lattice"] or {}).get("verdict") == "no-samples"]

    print("## 4. Verdict\n")
    print("Lattice gate (proposal): pooled 5 percent-trimmed mean worse by > "
          "%g percent and > %g ms; a first-slot share shift >= %g pp is flagged "
          "separately.\n" % (args.gate_pct, args.gate_abs_ms,
                              args.slot_shift_pp))
    for row in fast_rows:
        lat = row["lattice"]
        print("* %s: %s -- trimmed %s vs %s (%+.2f ms, %s), first-slot %s vs %s"
              "%s%s" % (
                  row["label"], lat["verdict"], fmt(lat["trim_a"]),
                  fmt(lat["trim_b"]), lat["delta"] or 0.0,
                  pct_delta_text(lat["delta_pct"]),
                  ("%.0f%%" % (100 * lat["first_slot_share_a"])
                   if lat["first_slot_share_a"] is not None else "-"),
                  ("%.0f%%" % (100 * lat["first_slot_share_b"])
                   if lat["first_slot_share_b"] is not None else "-"),
                  " (SLOT-SHIFT %+.0f pp)" % lat["first_slot_delta_pp"]
                  if lat["slot_shift"] else "",
                  " [no raw samples: old gate only]" if lat["verdict"] ==
                  "no-samples" else ""))
    if no_samples:
        print("\n* %d fast metric(s) carry no raw samples (old baseline JSON); "
              "re-run with the PERF-REL3 bench to score them on the lattice."
              % len(no_samples))
    if old_regressions:
        print("\nOld gate regressions: %s" %
              ", ".join(r["label"] for r in old_regressions))
    if lat_regressions:
        print("Lattice regressions: %s" %
              ", ".join(r["label"] for r in lat_regressions))
    if not old_regressions and not lat_regressions:
        print("\nNo regression under either gate.")

    if args.json:
        payload = {"label_a": args.label_a, "label_b": args.label_b,
                   "gate": {"pct": args.gate_pct, "abs_ms": args.gate_abs_ms,
                            "slot_shift_pp": args.slot_shift_pp,
                            "trim": args.trim,
                            "status": "proposal (PERF-REL3)"},
                   "rows": rows}
        Path(args.json).parent.mkdir(parents=True, exist_ok=True)
        Path(args.json).write_text(json.dumps(payload, indent=1, sort_keys=True)
                                   + "\n")
        print("\nJSON: %s" % args.json)

    return 1 if (old_regressions or lat_regressions) else 0


if __name__ == "__main__":
    sys.exit(main())
