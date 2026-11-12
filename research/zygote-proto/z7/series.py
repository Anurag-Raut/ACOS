#!/usr/bin/env python3
"""Z7 release timing: 1 Hz per-process Pss right after the children's first frames.

Answers the one question the lifetime cells can only bracket: does the optimised
arm release its launch garbage within ~1 s of the first frame, or does it wait
for ART's post-fork schedule (+2 s/+10 s)? The series starts as soon as the last
session's `ready` has been read (no tap request before it) and runs for
--seconds at 1 Hz with the Z6 smaps probe (per-process rollup + mapping
attribution + guest log lines).
"""
import argparse
import json
import os
import sys
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import common as C  # noqa: E402
import probe as z6_probe  # noqa: E402


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--arm", required=True, choices=C.ARMS)
    ap.add_argument("--n", type=int, required=True)
    ap.add_argument("--out", required=True)
    ap.add_argument("--json", required=True)
    ap.add_argument("--seconds", type=float, default=16.0)
    ap.add_argument("--app", default="counter", choices=["counter", "form-validation"])
    args = ap.parse_args()
    root = os.path.abspath(args.out)
    plan = os.path.join(root, "sessions")
    report = {"schema": "z7-series/1", "arm": args.arm, "n": args.n,
              "app": args.app, "seconds": args.seconds,
              "started_utc": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
              "art_bin": C.ART, "failures": []}
    procs, sessions, t0, overlay = C.spawn_cohort(args.arm, args.n, root, plan, args.app)
    report["knobs"] = overlay
    try:
        ready = C.read_all_ready(sessions, t0)
        t_ready = {k: v["t_ready_ms"] / 1000.0 for k, v in ready.items()}
        report["sessions_ready_ms"] = {str(k): v["t_ready_ms"] for k, v in ready.items()}
        for k, v in ready.items():
            if not v["reply"].get("ok"):
                report["failures"].append(f"session {k}: ready not ok: {v['reply']}")
        dropped = {k: 0 for k in sessions}
        seen = {k: 0 for k in sessions}

        def logs():
            fresh = []
            for k in sorted(sessions):
                try:
                    reply, _, _ = sessions[k].call({"cmd": "logs"})
                except BaseException:
                    continue
                ents = reply.get("entries") or []
                if (reply.get("dropped") or 0) != dropped[k]:
                    seen[k] = 0
                    dropped[k] = reply.get("dropped") or 0
                for e in ents[seen[k]:]:
                    msg = str(e.get("message", ""))
                    if any(s in msg for s in ("GC freed", "heap trim", "Using ",
                                              "concurrent copying", "EARLY_RELEASE")):
                        fresh.append({"session": k, "message": msg[:400]})
                seen[k] = len(ents)
            return fresh

        series = z6_probe.probe_series(os.getpid(), z4_classify, args.seconds,
                                       interval=1.0, log_fn=logs)
        report["series"] = series
        # Anchor: the last ready. Per-sample per-child Pss is in series[*].proc.
        report["session_ready_s"] = t_ready
    finally:
        report["launcher_exit"] = C.close_cohort(sessions, procs, bound=20.0)
        report["verdict"] = "pass" if not report["failures"] else "fail"
        C.write_json(args.json, report)
        print(f"[{args.arm}-n{args.n}] series done -> {args.json}", flush=True)


def z4_classify(pid):
    import drive as z4
    return z4.classify(pid)


if __name__ == "__main__":
    main()
