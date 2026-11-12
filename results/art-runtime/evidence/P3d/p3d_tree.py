#!/usr/bin/env python3
"""P3b/P3d: M7 tree latency, strict vs fast, plus post-tap freshness.

Imports the existing warm harness (Serve/Daemon) instead of forking it. One
held `acos serve` ART session, one counter.apk launch, no resets inside a phase.
"""
import argparse, json, os, shutil, sys, time
from pathlib import Path

HERE = Path(__file__).resolve().parent
REPO = HERE.parents[3]
sys.path.insert(0, str(REPO / "tools"))
sys.path.insert(0, str(REPO / "tools/art-runtime"))
import benchlib  # noqa: E402
import d2_warm  # noqa: E402

APK = REPO / "fixtures/build/apps/counter.apk"


def count_text(tree):
    return d2_warm.count_text(tree)


def one_tree(daemon, base, settle):
    args = {"base": base, "max_nodes": 2000}
    if settle != "strict":
        args["settle"] = settle
    r = daemon.call("acos_observe_tree", args)
    env = r["envelope"]
    value = env.get("result") or {}
    tree = value.get("tree") if isinstance(value.get("tree"), dict) else {}
    nodes = sum(1 for _ in d2_warm.agent_loop.walk(tree.get("nodes")))
    row = {
        "wall_ms": round(r["wall_ms"], 3),
        "ok": bool(env.get("ok")),
        "nodes": nodes,
        "text_nodes": d2_warm.text_nodes(tree),
        "count": count_text(tree),
        "truncated": bool(value.get("truncated")),
        "settle": value.get("settle"),
        "frame": value.get("frame"),
        "error": (env.get("error") or {}).get("code") if env.get("error") else None,
    }
    row["good"] = bool(row["ok"] and row["nodes"] > 0 and not row["truncated"])
    return row


def phase(daemon, base, settle, n, warmup):
    rows = []
    for i in range(warmup + n):
        row = one_tree(daemon, base, settle)
        row["warmup"] = i < warmup
        rows.append(row)
    return rows


def freshness(daemon, base, reps):
    rows = []
    for i in range(reps):
        expected = str(i + 1)
        tap = daemon.call("acos_tap", {"base": base, "id": "inc", "settle": "fast"})
        tenv = tap["envelope"]
        fast = one_tree(daemon, base, "fast")
        strict = one_tree(daemon, base, "strict")
        rows.append({
            "rep": i + 1,
            "expected": expected,
            "tap_ok": bool(tenv.get("ok")),
            "tap_state_changed": (tenv.get("result") or {}).get("state_changed"),
            "fast_count": fast["count"],
            "fast_wall_ms": fast["wall_ms"],
            "fast_settle": fast["settle"],
            "fast_good": fast["good"],
            "fast_matches": fast["count"] == expected,
            "strict_count": strict["count"],
            "strict_wall_ms": strict["wall_ms"],
            "strict_good": strict["good"],
            "strict_matches": strict["count"] == expected,
        })
    return rows


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--acos", required=True)
    ap.add_argument("--out", required=True)
    ap.add_argument("--scratch", required=True)
    ap.add_argument("--samples", type=int, default=30)
    ap.add_argument("--warmup", type=int, default=2)
    ap.add_argument("--fresh-reps", type=int, default=20)
    ap.add_argument("--skip-fresh", action="store_true")
    args = ap.parse_args()
    scratch = Path(args.scratch)
    if scratch.exists():
        shutil.rmtree(scratch)
    scratch.mkdir(parents=True)
    doc = {"schema": "acos-p3d-tree/1", "utc": d2_warm.utc(),
           "n": args.samples, "warmup": args.warmup,
           "apk_sha256": benchlib.sha256_file(APK),
           "acos": args.acos,
           "host_before": d2_warm.host_now(), "ok": False}
    serve = daemon = None
    try:
        serve = d2_warm.Serve(args.acos, scratch)
        daemon = serve.client()
        daemon.call("ping", {})
        base = {"request_id": "p3d-tree", "session_id": "s_p3d_tree"}
        launch = daemon.call("acos_launch", {"base": base, "apk": str(APK)})
        if not launch["envelope"].get("ok"):
            raise RuntimeError("launch failed: %s" % (launch["envelope"],))
        time.sleep(2.0)
        doc["strict"] = phase(daemon, base, "strict", args.samples, args.warmup)
        doc["fast"] = phase(daemon, base, "fast", args.samples, args.warmup)
        doc["freshness"] = [] if args.skip_fresh else freshness(
            daemon, base, args.fresh_reps)
        doc["ok"] = True
    except Exception as exc:  # noqa: BLE001
        doc["error"] = "%s: %s" % (type(exc).__name__, exc)
    finally:
        if daemon is not None:
            try:
                daemon.call("acos_destroy", {"base": {
                    "request_id": "p3d-destroy", "session_id": "s_p3d_tree"}})
            except Exception:  # noqa: BLE001
                pass
            try:
                daemon.close()
            except Exception:  # noqa: BLE001
                pass
        if serve is not None:
            doc["serve_rc"] = serve.close()
        time.sleep(0.5)
        doc["survivors"] = d2_warm.survivors(scratch)
        doc["host_after"] = d2_warm.host_now()
        doc["contaminated"] = not (d2_warm.host_ok(doc["host_before"])
                                   and d2_warm.host_ok(doc["host_after"]))
        doc["utc_finished"] = d2_warm.utc()
        Path(args.out).parent.mkdir(parents=True, exist_ok=True)
        with open(args.out, "w") as f:
            json.dump(doc, f, indent=2)
        print("p3d: wrote %s ok=%s" % (args.out, doc.get("ok")))
        shutil.rmtree(scratch, ignore_errors=True)
    return 0 if doc.get("ok") else 1


if __name__ == "__main__":
    sys.exit(main())
