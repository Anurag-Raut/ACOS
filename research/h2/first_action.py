#!/usr/bin/env python3
"""H2 first-action latency: launch -> `ready` -> the first agent action.

Each launch performs exactly one first action, cycled over
  strict tree (pixels:false) / fast tree (settle:fast, pixels:false) / tap
so the measurement is not polluted by a preceding tree.  The tap point comes
from a separate discovery launch (that tree is not counted).

Arms:
  off = ACOS_ART_EARLY_GC=0
  on  = ACOS_ART_EARLY_GC=1 (the H2 product default is on; the explicit value
        makes the arm label unambiguous)

Usage:
  first_action.py --app counter --arm on --n 20 --out DIR --json FILE
"""
import argparse
import json
import os
import statistics
import sys
import time

HERE = os.path.dirname(os.path.abspath(__file__))
Z7 = os.path.abspath(os.path.join(HERE, "..", "zygote-proto", "z7"))
sys.path.insert(0, Z7)
sys.path.insert(0, os.path.abspath(os.path.join(HERE, "..", "zygote-proto", "z4")))
import common as C  # noqa: E402
import drive as z4  # noqa: E402

ACTIONS = ("tree_strict", "tree_fast", "tap")


def stats(vals):
    vals = sorted(v for v in vals if v is not None)
    if not vals:
        return None
    def pct(p):
        i = min(len(vals) - 1, int(round((len(vals) - 1) * p)))
        return vals[i]
    return {"n": len(vals), "p50": round(pct(0.50), 1), "p95": round(pct(0.95), 1),
            "min": round(vals[0], 1), "max": round(vals[-1], 1),
            "mean": round(statistics.mean(vals), 1)}


def discover_tap(app):
    """One separate launch: find the tap node and return its centre."""
    profile = C.app_profile(app)
    root = os.path.join(os.environ["H2_SCRATCH"], "discover", app)
    plan = os.path.join(root, "sessions")
    os.environ["ACOS_ART_EARLY_GC"] = "0"
    procs, sessions, t0, _ = C.spawn_cohort("a", 1, root, plan, app)
    try:
        C.read_all_ready(sessions, t0)
        tree, _, _ = sessions[0].call({"cmd": "tree"})
        node = C.by_suffix(tree.get("tree"), profile["tap_suffix"])
        if node is None:
            raise SystemExit(f"{app}: no node {profile['tap_suffix']}")
        return C.center(node)
    finally:
        C.close_cohort(sessions, procs, bound=15.0)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--app", required=True, choices=["counter", "form-validation"])
    ap.add_argument("--arm", required=True, choices=["off", "on"])
    ap.add_argument("--n", type=int, default=20)
    ap.add_argument("--out", required=True)
    ap.add_argument("--json", required=True)
    args = ap.parse_args()
    os.environ.setdefault("H2_SCRATCH", os.path.abspath(args.out))
    knob = "0" if args.arm == "off" else "1"
    os.environ.pop("ACOS_ART_GC_DELAY_MS", None)
    x, y = discover_tap(args.app)
    os.environ["ACOS_ART_EARLY_GC"] = knob

    root = os.path.abspath(args.out)
    report = {
        "schema": "h2-first-action/1", "app": args.app, "arm": args.arm,
        "early_gc": knob, "n": args.n,
        "tap_point": [x, y], "started_utc": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
        "samples": [], "failures": [],
    }
    for i in range(args.n):
        action = ACTIONS[i % len(ACTIONS)]
        run = os.path.join(root, f"run-{i:02d}-{action}")
        plan = os.path.join(run, "sessions")
        procs, sessions, t0, overlay = C.spawn_cohort("a", 1, run, plan, args.app)
        rec = {"i": i, "action": action, "knobs": overlay}
        try:
            ready = C.read_all_ready(sessions, t0)
            rec["ready_ms"] = ready[0]["t_ready_ms"]
            if not ready[0]["reply"].get("ok"):
                rec["error"] = f"ready not ok: {ready[0]['reply']}"
                report["failures"].append(rec["error"])
                continue
            if action == "tree_strict":
                req = {"cmd": "tree", "settle": "strict", "pixels": False}
            elif action == "tree_fast":
                req = {"cmd": "tree", "settle": "fast", "pixels": False}
            else:
                req = {"cmd": "tap", "x": x, "y": y}
            reply, skipped, ms = sessions[0].call(req)
            rec.update({
                "latency_ms": round(ms, 2), "ok": bool(reply.get("ok")),
                "reply_code": reply.get("code"), "mode": reply.get("mode"),
                "settled": reply.get("settled"), "frame_seq": reply.get("frame_seq"),
                "task_since_spawn_ms": round((time.perf_counter() - t0) * 1000.0, 1),
            })
            if not rec["ok"]:
                report["failures"].append(f"{action}: {reply.get('code')}: {reply.get('error')}")
        except BaseException as e:
            rec["error"] = repr(e)
            report["failures"].append(f"{action}: {e!r}")
        finally:
            close = C.close_cohort(sessions, procs, bound=15.0)
            rec["close_exit"] = {k: v.get("exit") for k, v in close.items()}
            text = ""
            for suffix in ("out", "err"):
                gc = os.path.join(run, "s0", f"guest-0.{suffix}")
                try:
                    text += open(gc, "r", errors="replace").read()
                except OSError:
                    pass
            rec["early_release_marker"] = "ACOSLAUNCH EARLY_RELEASE done" in text
            report["samples"].append(rec)
            print(f"[{args.app}-{args.arm}] {i:02d} {action:11s} "
                  f"ready={rec.get('ready_ms')}ms latency={rec.get('latency_ms')}ms "
                  f"ok={rec.get('ok')} gc={rec.get('early_release_marker')}", flush=True)

    rows = [r for r in report["samples"] if r.get("latency_ms") is not None]
    report["summary"] = {
        "all": stats([r["latency_ms"] for r in rows]),
        "by_action": {a: stats([r["latency_ms"] for r in rows if r["action"] == a])
                      for a in ACTIONS},
        "ready": stats([r.get("ready_ms") for r in rows]),
        "failures": len(report["failures"]),
    }
    report["verdict"] = "pass" if not report["failures"] else "fail"
    os.makedirs(os.path.dirname(os.path.abspath(args.json)), exist_ok=True)
    with open(args.json, "w") as f:
        json.dump(report, f, indent=1, sort_keys=True)
    print(f"[{args.app}-{args.arm}] verdict={report['verdict']} "
          f"all={report['summary']['all']} by={report['summary']['by_action']} -> {args.json}",
          flush=True)


if __name__ == "__main__":
    main()
