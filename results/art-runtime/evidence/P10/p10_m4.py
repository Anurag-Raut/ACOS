#!/usr/bin/env python3
"""P10: M4 warm launch via P7's own harness (phase_m4), pointed at a given build.

P7's p7.py hardcodes target/release; this wrapper only overrides BIN/ART_BIN and
reuses its verified-reference launch/destroy cohort unchanged.
"""
import argparse
import json
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
REPO = HERE.parents[3]
P7 = REPO / "results/art-runtime/evidence/P7"
sys.path.insert(0, str(P7))
import p7  # noqa: E402


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--acos", required=True)
    ap.add_argument("--acos-art", required=True)
    ap.add_argument("--out", required=True)
    ap.add_argument("--scratch", required=True)
    ap.add_argument("--reps", type=int, default=20)
    args = ap.parse_args()
    p7.BIN = Path(args.acos)
    p7.ART_BIN = Path(args.acos_art)
    ref = json.load(open(P7 / "run/ref.json"))
    scratch = Path(args.scratch)
    scratch.mkdir(parents=True, exist_ok=True)
    payload = p7.phase_m4(scratch, args.reps, ref, Path(args.out))
    rows = payload["rows"]
    ok = sum(1 for r in rows if r.get("ok"))
    launches = [r.get("launch_wall_ms") for r in rows if r.get("launch_wall_ms")]
    total = [r.get("m_ms") for r in rows if r.get("m_ms")]
    pc = p7.benchlib.percentile
    summary = {
        "n": len(rows), "verified": ok,
        "launch_p50_ms": pc(launches, 50),
        "launch_p95_ms": pc(launches, 95),
        "total_p50_ms": pc(total, 50),
        "total_p95_ms": pc(total, 95),
    }
    print(json.dumps(summary, indent=2))


if __name__ == "__main__":
    main()
