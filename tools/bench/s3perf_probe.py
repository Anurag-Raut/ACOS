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
MARK_KEYS = ("first-traversal-skipped", "first-traversal-drew",
             "visibility-redelivered", "activity-not-ready")


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


def newest_marker(scratch):
    """The newest `activity-resumed.marker` under the probe scratch.

    The guest publishes the skipped-draw evidence there (`draw_skip=`,
    `first_traversal=`); the probe reports what it says, not what it guesses.
    """
    try:
        files = sorted(scratch.rglob("activity-resumed.marker"),
                       key=lambda p: p.stat().st_mtime)
    except OSError:
        return None
    return files[-1] if files else None


def marker_fields(path):
    fields = {}
    if path is None:
        return fields
    try:
        for line in Path(path).read_text(errors="replace").splitlines():
            if "=" in line:
                k, v = line.split("=", 1)
                fields[k.strip()] = v.strip()
    except OSError:
        pass
    return fields


def forced_redraw(row):
    """Did this launch force a second traversal / replay visibility (the
    int-s3ab cost)? Main and this fix must both answer False."""
    return any(("requesting one redraw" in m) or m.startswith("visibility-redelivered")
               for m in row["marks"])


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
            mf = marker_fields(newest_marker(scratch))
            row = {
                "i": i,
                "ok": bool(env.get("ok")),
                "wall_ms": r["wall_ms"],
                "code": ((env.get("error") or {}) or {}).get("code"),
                "settle_ms": (value.get("settle") or {}).get("total_ms"),
                "marks": mk,
                "skip": [m for m in mk if m.startswith("first-traversal-skipped")],
                "marker_draw_skip": mf.get("draw_skip"),
                "marker_first_traversal": mf.get("first_traversal"),
            }
            row["forced_redraw"] = forced_redraw(row)
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
        "draw_skip_evidence": sum(1 for r in measured
                                  if r.get("marker_draw_skip")),
        "marker_draw_skips": sorted({r["marker_draw_skip"] for r in measured
                                     if r.get("marker_draw_skip")}),
        "forced_redraws": sum(1 for r in measured if r["forced_redraw"]),
        "ready_path": ("main-identical" if not any(r["forced_redraw"] for r in measured)
                       else "forced-traversal"),
        "fetch_failures": sum(1 for r in measured
                              if r.get("code") == "POST_RESUME_NOT_READY"),
        "p50_ms": statistics.median(w) if w else None,
        "min_ms": w[0] if w else None,
        "max_ms": w[-1] if w else None,
    }
    Path(args.out).write_text(json.dumps(doc, indent=2) + "\n")
    print(json.dumps({k: doc[k] for k in (
        "measured", "skips", "skip_reasons", "draw_skip_evidence",
        "marker_draw_skips", "forced_redraws", "ready_path", "fetch_failures",
        "p50_ms", "min_ms", "max_ms")}))


if __name__ == "__main__":
    main()
