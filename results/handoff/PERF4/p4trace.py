#!/usr/bin/env python3
"""PERF4: one form-fast loop with the host input trace on, for attribution.

Runs `acos serve` on a form session, does N warm taps and M measured taps, and
leaves the serve log + `ACOS_ART_INPUT_TRACE` file in --out for inspection.
Shared slot only (this is not a gate).
"""
import argparse
import os
import shutil
import sys
from pathlib import Path

WT = Path(__file__).resolve().parent.parent.parent.parent
sys.path.insert(0, str(WT / "tools"))
sys.path.insert(0, str(WT / "tools/art-runtime"))
sys.path.insert(0, str(WT / "tools/bench"))
import d2_warm  # noqa: E402

FORM = WT / "fixtures/build/apps/form-validation.apk"


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--acos", required=True)
    ap.add_argument("--out", required=True)
    ap.add_argument("--taps", type=int, default=3)
    ap.add_argument("--env", action="append", default=[])
    args = ap.parse_args()
    out = Path(args.out)
    if out.exists():
        shutil.rmtree(out)
    out.mkdir(parents=True)
    for spec in args.env:
        k, _, v = spec.partition("=")
        os.environ[k] = v
    os.environ["ACOS_ART_INPUT_TRACE"] = str(out / "input-trace.log")
    (out / "scratch").mkdir(parents=True, exist_ok=True)
    serve = d2_warm.Serve(args.acos, out / "scratch", max_sessions=4)
    d = serve.client()
    try:
        d.call("ping", {})
        r = d.call("acos_launch", {"base": "b", "apk": str(FORM), "settle": "strict"})
        print("launch ok:", (r.get("envelope") or {}).get("ok"))
        for i in range(3):
            d.call("acos_reset", {"base": "b", "settle": "fast"})
            d.call("acos_tap", {"base": "b", "id": "submit", "settle": "fast"})
            d.call("acos_observe_tree", {"base": "b", "settle": "strict"})
        rows = []
        for i in range(args.taps):
            d.call("acos_reset", {"base": "b", "settle": "fast"})
            tr = d.call("acos_tap", {"base": "b", "id": "submit", "settle": "fast"})
            tres = (tr.get("envelope") or {}).get("result") or {}
            inp = ((tres.get("timing") or {}).get("input") or {})
            rows.append({"wall": tr.get("wall_ms"),
                         "settle": tres.get("settle"),
                         "guest_command_ms": inp.get("guest_command_ms")})
            d.call("acos_observe_tree", {"base": "b", "settle": "strict"})
        for row in rows:
            print(row)
    finally:
        try:
            d.close()
        finally:
            serve.close()


if __name__ == "__main__":
    main()
