#!/usr/bin/env python3
"""P10: M8 strict counter tap, direct cohort (P1's verb and state).

One held `acos serve` ART session, counter.apk: launch (strict), then N
`acos_tap {id:inc, settle:strict}` reps on the same session, recording the
client wall and the reply's `settle` block. No reset inside the cohort (each rep
increments the counter); this is the strict tap cost P1 measured at 1064.7 ms
p50, i.e. the post-tap ripple tail the strict quiet rule waits out.
"""
import argparse
import json
import shutil
import sys
import time
from pathlib import Path

HERE = Path(__file__).resolve().parent
REPO = HERE.parents[3]
sys.path.insert(0, str(REPO / "tools"))
sys.path.insert(0, str(REPO / "tools/art-runtime"))
import benchlib          # noqa: E402
import d2_warm           # noqa: E402

APK = REPO / "fixtures/build/apps/counter.apk"


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--acos", required=True)
    ap.add_argument("--out", required=True)
    ap.add_argument("--scratch", required=True)
    ap.add_argument("--reps", type=int, default=20)
    args = ap.parse_args()
    scratch = Path(args.scratch)
    if scratch.exists():
        shutil.rmtree(scratch)
    scratch.mkdir(parents=True)
    doc = {"schema": "acos-p10-m8/1", "utc": d2_warm.utc(),
           "apk_sha256": benchlib.sha256_file(APK), "acos": args.acos,
           "host_before": d2_warm.host_now(), "ok": False}
    serve = daemon = None
    rows = []
    try:
        serve = d2_warm.Serve(args.acos, scratch)
        daemon = serve.client()
        daemon.call("ping", {})
        base = {"request_id": "p10-m8", "session_id": "s_p10_m8"}
        launch = daemon.call("acos_launch", {"base": base, "apk": str(APK)})
        if not launch["envelope"].get("ok"):
            raise RuntimeError("launch failed: %s" % (launch["envelope"],))
        # one discarded warm-up tap (same state P1 measured after warm-up)
        daemon.call("acos_tap", {"base": base, "id": "inc", "settle": "strict"})
        time.sleep(0.5)
        for i in range(args.reps):
            r = daemon.call("acos_tap", {"base": base, "id": "inc",
                                         "settle": "strict"})
            env = r.get("envelope") or {}
            res = env.get("result") or {}
            settle = res.get("settle") or {}
            rows.append({
                "rep": i + 1,
                "ok": bool(env.get("ok")),
                "wall_ms": round(r["wall_ms"], 3),
                "ack_ms": settle.get("ack_ms"),
                "first_change_ms": settle.get("first_change_ms"),
                "frame_ms": settle.get("frame_ms"),
                "state_changed": settle.get("state_changed"),
                "bound": settle.get("bound"),
            })
        doc["rows"] = rows
        good = [r["wall_ms"] for r in rows if r["ok"]]
        doc["summary"] = {
            "n": len(rows), "ok": len(good),
            "p50_ms": round(benchlib.percentile(good, 50), 2) if good else None,
            "p95_ms": round(benchlib.percentile(good, 95), 2) if good else None,
            "ack_p50_ms": benchlib.percentile(
                [r["ack_ms"] for r in rows if r["ack_ms"] is not None], 50),
            "frame_p50_ms": benchlib.percentile(
                [r["frame_ms"] for r in rows if r["frame_ms"] is not None], 50),
        }
        doc["ok"] = True
    except Exception as exc:  # noqa: BLE001
        doc["error"] = "%s: %s" % (type(exc).__name__, exc)
    finally:
        if daemon is not None:
            try:
                daemon.call("acos_destroy", {"base": {
                    "request_id": "p10-destroy", "session_id": "s_p10_m8"}})
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
        print("p10-m8: wrote %s ok=%s" % (args.out, doc.get("ok")))
        shutil.rmtree(scratch, ignore_errors=True)
    return 0 if doc.get("ok") else 1


if __name__ == "__main__":
    sys.exit(main())
