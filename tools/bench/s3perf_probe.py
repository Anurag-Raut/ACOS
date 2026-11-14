#!/usr/bin/env python3
"""S3PERF: per-launch readiness-path probe.

Runs N sequential counter launches against one `acos serve` and records, per
launch, the guest readiness marks (does the draw-skip path fire? with which
reason?) plus the launch wall time. Sequential single-session launches keep
ACOSLAUNCH_GUEST_STDOUT_FILE meaningful (the launcher recreates it per launch).

usage: probe.py --acos <binary> --out <json> --scratch <dir> [--n 10] [--mode strict]
"""
import argparse
import json
import os
import statistics
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
# Default: tools/bench/ -> the repo root. ACOS_PROBE_REPO overrides (a worktree run).
REPO = Path(os.environ.get("ACOS_PROBE_REPO", str(HERE.parents[1]))).resolve()
sys.path.insert(0, str(REPO / "tools"))
sys.path.insert(0, str(REPO / "tools/art-runtime"))

import agent_loop  # noqa: E402
import d2_warm  # noqa: E402

COUNTER = Path("/home/Qui/Projects/acos/fixtures/build/apps/counter.apk")
MARK_KEYS = ("first-traversal-skipped", "visibility-redelivered", "activity-not-ready")


def marks(path):
    out = []
    try:
        for line in Path(path).read_text(errors="replace").splitlines():
            if "ACOSLAUNCH MARK" in line:
                for k in MARK_KEYS:
                    if k in line:
                        out.append(line.split("ACOSLAUNCH MARK", 1)[1].strip())
    except OSError:
        pass
    return out


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--acos", required=True)
    ap.add_argument("--out", required=True)
    ap.add_argument("--scratch", required=True)
    ap.add_argument("--n", type=int, default=10)
    ap.add_argument("--mode", default="strict")
    ap.add_argument("--apk", default=str(COUNTER))
    args = ap.parse_args()

    scratch = Path(args.scratch)
    if scratch.exists():
        import shutil
        shutil.rmtree(scratch)
    scratch.mkdir(parents=True)
    guest_out = scratch / "guest-stdout.log"
    os.environ["ACOSLAUNCH_GUEST_STDOUT_FILE"] = str(guest_out)

    rows = []
    serve = d2_warm.Serve(args.acos, scratch, max_sessions=2)
    try:
        d = serve.client()
        d.call("ping", {})
        for i in range(args.n + 1):  # rep 0 is the warm-up, discarded
            base = {"request_id": "s3perf-%d" % i, "session_id": "s_s3perf_%d" % i}
            r = d.call("acos_launch", {"base": base, "apk": args.apk,
                                       "settle": args.mode,
                                       "animations": "on"})
            env = r.get("envelope") or {}
            value = env.get("result") or {}
            mk = marks(guest_out)
            row = {
                "i": i,
                "ok": bool(env.get("ok")),
                "wall_ms": r["wall_ms"],
                "code": ((env.get("error") or {}) or {}).get("code"),
                "settle_ms": (value.get("settle") or {}).get("total_ms"),
                "marks": mk,
                "skip": [m for m in mk if m.startswith("first-traversal-skipped")],
            }
            rows.append(row)
            print(json.dumps(row), flush=True)
            d.call("acos_destroy", {"base": base})
    finally:
        serve.close()

    measured = [r for r in rows if r["i"] > 0 and r["ok"]]
    w = sorted(r["wall_ms"] for r in measured)
    doc = {
        "acos": args.acos,
        "mode": args.mode,
        "n": args.n,
        "rows": rows,
        "measured": len(measured),
        "skips": sum(1 for r in measured if r["skip"]),
        "skip_reasons": sorted({s for r in measured for s in r["skip"]}),
        "p50_ms": statistics.median(w) if w else None,
        "min_ms": w[0] if w else None,
        "max_ms": w[-1] if w else None,
    }
    Path(args.out).write_text(json.dumps(doc, indent=2) + "\n")
    print(json.dumps({k: doc[k] for k in ("measured", "skips", "skip_reasons",
                                          "p50_ms", "min_ms", "max_ms")}))


if __name__ == "__main__":
    main()
