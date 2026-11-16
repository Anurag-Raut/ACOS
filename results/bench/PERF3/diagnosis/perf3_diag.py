#!/usr/bin/env python3
"""PERF3 diagnostic: split M8 form fast into reset + tap walls, per arm.

Not a gate. Run through run-exclusive --exclusive (bench owner).
"""
import argparse
import json
import os
import statistics
import sys
import time
from pathlib import Path

WT = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(WT / "tools"))
sys.path.insert(0, str(WT / "tools/art-runtime"))
sys.path.insert(0, str(WT / "tools/bench"))
import d2_warm  # noqa: E402
import bench_reply  # noqa: E402

FORM = WT / "fixtures/build/apps/form-validation.apk"


def stats(xs):
    xs = sorted(xs)
    return {
        "n": len(xs),
        "mean": round(statistics.mean(xs), 3),
        "p50": round(xs[len(xs) // 2], 3),
        "p5": round(xs[int(len(xs) * 0.05)], 3),
        "p95": round(xs[int(len(xs) * 0.95)], 3),
        "min": round(xs[0], 3),
        "max": round(xs[-1], 3),
    }


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--acos", required=True)
    ap.add_argument("--scratch", required=True)
    ap.add_argument("--reps", type=int, default=80)
    ap.add_argument("--mode", default="reset-tap", choices=["reset-tap", "tap", "reset", "bench-form"])
    ap.add_argument("--out", required=True)
    args = ap.parse_args()
    scratch = Path(args.scratch)
    if scratch.exists():
        import shutil
        shutil.rmtree(scratch)
    scratch.mkdir(parents=True)
    serve = d2_warm.Serve(args.acos, scratch, max_sessions=8)
    d = serve.client()
    rows = []
    try:
        d.call("ping", {})
        r = d.call("acos_launch", {"base": "f", "apk": str(FORM), "settle": "strict"})
        env = r.get("envelope") or {}
        if not env.get("ok"):
            raise SystemExit("launch failed: %s" % env)
        # warmup
        for i in range(5):
            d.call("acos_reset", {"base": "f", "settle": "fast"})
            d.call("acos_tap", {"base": "f", "id": "submit", "settle": "fast"})
            if args.mode == "bench-form":
                d.call("acos_observe_tree", {"base": "f", "settle": "strict"})
        for i in range(args.reps):
            rr = d.call("acos_reset", {"base": "f", "settle": "fast"})
            oenv = (rr.get("envelope") or {}).get("result") or {}
            tr = d.call("acos_tap", {"base": "f", "id": "submit", "settle": "fast",
                                     "diff": True})
            tenv = tr.get("envelope") or {}
            tres, settle = bench_reply.require_tap(tenv, "diag-%d" % i)
            obs = None
            if args.mode == "bench-form":
                orr = d.call("acos_observe_tree", {"base": "f", "settle": "strict"})
                obs = orr["wall_ms"]
            rows.append({
                "i": i,
                "reset_ms": rr["wall_ms"],
                "tap_ms": tr["wall_ms"],
                "observe_ms": obs,
                "reset_tree_path": oenv.get("tree_path"),
                "reset_tree_nodes": oenv.get("tree_node_count"),
                "ack_ms": settle.get("ack_ms"),
                "first_change_ms": settle.get("first_change_ms"),
                "frame_ms": settle.get("frame_ms"),
                "settled": settle.get("settled"),
                "state_changed": tres.get("state_changed"),
                "reset_ok": bool((rr.get("envelope") or {}).get("ok")),
                "timing": ((tres.get("timing") or {}).get("input") if isinstance(tres.get("timing"), dict) else None),
                "tap_bytes": tr.get("response_bytes"),
                "reset_bytes": rr.get("response_bytes"),
            })
    finally:
        try:
            d.close()
        finally:
            serve.close()
    doc = {
        "acos": args.acos,
        "reps": len(rows),
        "rows": rows,
        "reset": stats([r["reset_ms"] for r in rows]),
        "tap": stats([r["tap_ms"] for r in rows]),
        "ack": stats([r["ack_ms"] for r in rows if r["ack_ms"] is not None]),
        "first_change": stats([r["first_change_ms"] for r in rows if r["first_change_ms"] is not None]),
        "observe": stats([r["observe_ms"] for r in rows if r.get("observe_ms") is not None]),
        "tree_paths": sum(1 for r in rows if r.get("reset_tree_path")),
    }
    Path(args.out).write_text(json.dumps(doc, indent=2))
    print(json.dumps({k: doc[k] for k in ("reset", "tap", "ack", "first_change")}, indent=2))


if __name__ == "__main__":
    main()
