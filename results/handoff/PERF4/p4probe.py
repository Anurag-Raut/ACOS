#!/usr/bin/env python3
"""PERF4 diagnostic: split the M8 fast tap loop into its measured parts.

Not a gate, not an exclusive benchmark: meant to be run inside a *shared*
run-exclusive slot (5 reps, indicative) so suspect toggles can be compared
without blocking the bench owner. Derived from PERF3's
``results/bench/PERF3/diagnosis/perf3_diag.py`` (same Serve client, same loops
as ``tools/bench/acos_bench.py``'s M8 form/counter fast loops) and extended to
record the reply's own ``timing.input`` split (guest_command_ms / input_ack_ms /
settle_estimate_ms) plus every settle field the bench records, so a delta can be
attributed to delivery, the probe sample, the C1 commit or the reply.

Usage:
  p4probe.py --arm name=/path/to/acos ... --metric form|counter --reps 20 \
             --scratch DIR --out FILE
Arms are interleaved per rep (A, B, A, B, ...) to cancel host drift.
"""
import argparse
import json
import os
import shutil
import statistics
import sys
from pathlib import Path

WT = Path(__file__).resolve().parent.parent.parent.parent
sys.path.insert(0, str(WT / "tools"))
sys.path.insert(0, str(WT / "tools/art-runtime"))
sys.path.insert(0, str(WT / "tools/bench"))
import d2_warm  # noqa: E402
import bench_reply  # noqa: E402

FORM = WT / "fixtures/build/apps/form-validation.apk"
COUNTER = WT / "fixtures/build/apps/counter.apk"


def trimmed(xs, frac=0.05):
    xs = sorted(x for x in xs if x is not None)
    if not xs:
        return None
    k = int(len(xs) * frac)
    keep = xs[k:len(xs) - k] or xs
    return round(statistics.mean(keep), 3)


def stats(xs):
    xs = sorted(x for x in xs if x is not None)
    if not xs:
        return {"n": 0}
    return {
        "n": len(xs),
        "trimmed": trimmed(xs),
        "mean": round(statistics.mean(xs), 3),
        "p50": round(xs[len(xs) // 2], 3),
        "min": round(xs[0], 3),
        "max": round(xs[-1], 3),
    }


def sample_fields(tres, settle, wall):
    timing = tres.get("timing") if isinstance(tres.get("timing"), dict) else {}
    inp = timing.get("input") if isinstance(timing.get("input"), dict) else {}
    return {
        "wall_ms": wall,
        "ack_ms": settle.get("ack_ms"),
        "first_change_ms": settle.get("first_change_ms"),
        "frame_ms": settle.get("frame_ms"),
        "settle_mode": settle.get("mode"),
        "bound": settle.get("bound"),
        "settled": settle.get("settled"),
        "state_changed": tres.get("state_changed"),
        "guest_command_ms": inp.get("guest_command_ms"),
        "input_ack_ms": inp.get("input_ack_ms"),
        "settle_estimate_ms": inp.get("settle_estimate_ms"),
        "absorb_ms": inp.get("absorb_ms"),
        "total_ms": timing.get("total_ms"),
        "recovery_polls": inp.get("recovery_polls"),
        "host_recovery_ms": inp.get("host_recovery_ms"),
        "response_bytes": tres.get("_response_bytes"),
        "handled": tres.get("handled"),
    }


def run_arm(acos, scratch, metric, reps, tag):
    scratch = Path(scratch)
    if scratch.exists():
        shutil.rmtree(scratch)
    scratch.mkdir(parents=True)
    serve = d2_warm.Serve(acos, scratch, max_sessions=8)
    d = serve.client()
    rows, walls = [], []
    try:
        d.call("ping", {})
        apk = FORM if metric == "form" else COUNTER
        r = d.call("acos_launch", {"base": "b", "apk": str(apk), "settle": "strict"})
        env = r.get("envelope") or {}
        if not env.get("ok"):
            raise SystemExit("%s launch failed: %s" % (tag, env))
        tap_args = {"base": "b", "id": "submit" if metric == "form" else "inc",
                    "settle": "fast"}
        for _ in range(5):  # warm-up, discarded (same as the bench)
            if metric == "form":
                d.call("acos_reset", {"base": "b", "settle": "fast"})
            d.call("acos_tap", tap_args)
            if metric == "form":
                d.call("acos_observe_tree", {"base": "b", "settle": "strict"})
        for i in range(reps):
            if metric == "form":
                rr = d.call("acos_reset", {"base": "b", "settle": "fast"})
                if not (rr.get("envelope") or {}).get("ok"):
                    rows.append({"i": i, "error": "reset"})
                    continue
            tr = d.call("acos_tap", tap_args)
            tenv = tr.get("envelope") or {}
            try:
                tres, settle = bench_reply.require_tap(tenv, "%s-%d" % (tag, i))
            except Exception as exc:  # noqa: BLE001
                rows.append({"i": i, "error": "tap: %s" % exc})
                continue
            row = sample_fields(tres, settle, tr.get("wall_ms"))
            row["i"] = i
            rows.append(row)
            walls.append(tr.get("wall_ms"))
            if metric == "form":
                d.call("acos_observe_tree", {"base": "b", "settle": "strict"})
    finally:
        try:
            d.close()
        finally:
            serve.close()
    return rows


def summarise(rows):
    keys = ["wall_ms", "ack_ms", "first_change_ms", "frame_ms", "guest_command_ms",
            "input_ack_ms", "settle_estimate_ms", "absorb_ms", "recovery_polls"]
    out = {k: stats([r.get(k) for r in rows]) for k in keys}
    out["errors"] = [r for r in rows if r.get("error")]
    out["bounds"] = sorted({str(r.get("bound")) for r in rows if r.get("bound")})
    out["modes"] = sorted({str(r.get("settle_mode")) for r in rows if r.get("settle_mode")})
    return out


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--arm", action="append", required=True,
                    help="name=/path/to/acos (repeat for A/B)")
    ap.add_argument("--metric", default="form", choices=["form", "counter"])
    ap.add_argument("--reps", type=int, default=20)
    ap.add_argument("--rounds", type=int, default=1, help="A/B/A/B rounds")
    ap.add_argument("--scratch", required=True)
    ap.add_argument("--out", required=True)
    args = ap.parse_args()
    arms = []
    for spec in args.arm:
        name, _, path = spec.partition("=")
        arms.append((name, path))
    per = {name: [] for name, _ in arms}
    for rnd in range(args.rounds):
        for name, path in arms:
            rows = run_arm(path, Path(args.scratch) / ("%s-r%d" % (name, rnd)),
                           args.metric, args.reps, name)
            per[name].extend(rows)
    doc = {"metric": args.metric, "reps_per_arm": args.reps, "rounds": args.rounds,
           "arms": {name: {"binary": path, "summary": summarise(per[name]),
                           "rows": per[name]} for name, path in arms}}
    Path(args.out).write_text(json.dumps(doc, indent=2))
    table = {name: doc["arms"][name]["summary"] for name, _ in arms}
    print(json.dumps({n: {k: v.get("trimmed") for k, v in s.items()
                          if isinstance(v, dict)} for n, s in table.items()},
                     indent=2))


if __name__ == "__main__":
    main()
