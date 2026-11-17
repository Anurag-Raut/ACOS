#!/usr/bin/env python3
"""PERF3 interleaved block probe: alternate arms within one acquisition.

Each block is a fresh serve + form session + N bench-form iterations. Arms may
differ by binary and by environment (e.g. ACOS_PERF3_NO_WRITE). Diagnostic only.
"""
import argparse
import json
import os
import statistics
import sys
import shutil
from pathlib import Path

WT = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(WT / "tools"))
sys.path.insert(0, str(WT / "tools/art-runtime"))
sys.path.insert(0, str(WT / "tools/bench"))
import d2_warm  # noqa: E402
import bench_reply  # noqa: E402

FORM = WT / "fixtures/build/apps/form-validation.apk"


def run_block(acos, scratch, reps):
    if scratch.exists():
        shutil.rmtree(scratch)
    scratch.mkdir(parents=True)
    serve = d2_warm.Serve(acos, scratch, max_sessions=8)
    d = serve.client()
    rows = []
    try:
        d.call("ping", {})
        r = d.call("acos_launch", {"base": "f", "apk": str(FORM), "settle": "strict"})
        if not (r.get("envelope") or {}).get("ok"):
            raise RuntimeError("launch failed")
        for _ in range(3):
            d.call("acos_reset", {"base": "f", "settle": "fast"})
            d.call("acos_tap", {"base": "f", "id": "submit", "settle": "fast"})
            d.call("acos_observe_tree", {"base": "f", "settle": "strict"})
        for i in range(reps):
            rr = d.call("acos_reset", {"base": "f", "settle": "fast"})
            tr = d.call("acos_tap", {"base": "f", "id": "submit", "settle": "fast"})
            tres, settle = bench_reply.require_tap(tr.get("envelope") or {}, "blk")
            d.call("acos_observe_tree", {"base": "f", "settle": "strict"})
            rows.append({"i": i, "reset_ms": rr["wall_ms"], "tap_ms": tr["wall_ms"],
                         "first_change_ms": settle.get("first_change_ms"),
                         "frame_ms": settle.get("frame_ms"),
                         "timing": ((tres.get("timing") or {}).get("input")
                                    if isinstance(tres.get("timing"), dict) else None)})
    finally:
        try:
            d.close()
        finally:
            serve.close()
    return rows


def summarize(rows):
    vals = sorted(r["tap_ms"] for r in rows)
    n = len(vals)
    trim = vals[int(n * 0.05):n - int(n * 0.05)] if n >= 20 else vals
    return {"n": n, "mean": round(statistics.mean(vals), 3),
            "p50": round(vals[n // 2], 3),
            "p95": round(vals[int(n * 0.95)], 3),
            "trimmed": round(statistics.mean(trim), 3)}


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--acos-a", required=True)
    ap.add_argument("--acos-b", required=True)
    ap.add_argument("--env-a", default="")
    ap.add_argument("--env-b", default="")
    ap.add_argument("--label-a", default="A")
    ap.add_argument("--label-b", default="B")
    ap.add_argument("--blocks", type=int, default=6)
    ap.add_argument("--reps", type=int, default=20)
    ap.add_argument("--scratch", required=True)
    ap.add_argument("--out", required=True)
    args = ap.parse_args()
    base_env = dict(os.environ)
    arms = [
        (args.label_a, args.acos_a, args.env_a),
        (args.label_b, args.acos_b, args.env_b),
    ]
    allrows = {args.label_a: [], args.label_b: []}
    for b in range(args.blocks):
        for label, acos, envs in arms:
            for key in list(os.environ):
                if key == "ACOS_PERF3_NO_WRITE" or key == "ACOS_PERF3_WRITE_MODE":
                    del os.environ[key]
            os.environ.update(base_env)
            if envs:
                if "=" in envs:
                    k, v = envs.split("=", 1)
                    os.environ[k] = v
                else:
                    os.environ["ACOS_PERF3_WRITE_MODE"] = envs
            scratch = Path(args.scratch) / ("blk-%d-%s" % (b, label))
            rows = run_block(acos, scratch, args.reps)
            allrows[label].extend(rows)
            print("block", b, label, summarize(rows), flush=True)
    doc = {"labels": {k: summarize(v) for k, v in allrows.items()},
           "rows": allrows}
    Path(args.out).write_text(json.dumps(doc, indent=2))
    print(json.dumps(doc["labels"], indent=2))


if __name__ == "__main__":
    main()
