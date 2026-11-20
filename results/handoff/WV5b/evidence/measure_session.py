#!/usr/bin/env python3
"""WV5b metric: what the lazy WebView gate costs a session that never creates a
WebView.

Arms (5 reps each, alternating, same command, same host):

  gate-on   the launch as it now is: `launch_session` starts this session's
            socket listener (nothing else, no broker, no Chromium)
  gate-off  `ACOS_WEBVIEW_GATE=0`: no listener at all (the pre-WV5b shape)

Per rep it records

  * `wall_ms`      the launch's own number from its JSON result (`wall_ms`)
  * `self_s`       the wall clock around `acos-art launch` (process spawn + exit)
  * `pss_peak_kib` the launcher process's peak PSS, sampled from
                   /proc/<pid>/smaps_rollup every 50 ms while it runs
  * `rc`           exit code, and the JSON's outcome/code

The gate's own start cost is measured separately and exactly by
`cargo test -p acos-webview --test gate_cost` (it reads this process's PSS and
threads before/after five `Gate::start`s).

Run it under the shared lock, exactly like any other ART run:

  tools/art-runtime/run-exclusive.sh --task WV5b -- tools/guarded.sh --mem 8G \
    --time 600 -- python3 results/handoff/WV5b/evidence/measure_session.py 5
"""

import json
import os
import statistics
import subprocess
import sys
import time

REPS = int(sys.argv[1]) if len(sys.argv) > 1 else 5
WT = os.environ.get("WT", os.getcwd())
ART = os.path.join(WT, ".cache/wv5b-target/release/acos-art")
APK = os.path.join(WT, "fixtures/build/apps/acos-counter.apk")
OUT = os.path.join(WT, ".cache/wv5b-run")

BASE_ENV = {
    k: v
    for k, v in os.environ.items()
    if k not in ("ACOS_HOME", "ACOS_WEBVIEW_GATE", "ACOS_WEBVIEW_SOCKET_DIR")
}
BASE_ENV["ACOS_FONT_DIR"] = "/home/Qui/Projects/acos/.cache/boot/android-34/fonts"


def pss_kib(pid):
    try:
        with open(f"/proc/{pid}/smaps_rollup") as f:
            for line in f:
                if line.startswith("Pss:"):
                    return int(line.split()[1])
    except OSError:
        return 0
    return 0


def one(arm):
    env = dict(BASE_ENV)
    if arm == "gate-off":
        env["ACOS_WEBVIEW_GATE"] = "0"
    cmd = [ART, "launch", "--apk", APK, "--until", "application-created"]
    t0 = time.monotonic()
    proc = subprocess.Popen(
        cmd, env=env, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True
    )
    peak = 0
    while proc.poll() is None:
        peak = max(peak, pss_kib(proc.pid))
        time.sleep(0.05)
    out, err = proc.communicate()
    self_s = time.monotonic() - t0
    wall_ms = None
    outcome = code = None
    try:
        doc = json.loads(out[out.index("{") :]) if "{" in out else {}
        wall_ms = doc.get("wall_ms")
        outcome = doc.get("outcome")
        code = doc.get("code")
    except (ValueError, json.JSONDecodeError):
        pass
    return {
        "arm": arm,
        "rc": proc.returncode,
        "wall_ms": wall_ms,
        "self_s": round(self_s, 3),
        "pss_peak_kib": peak,
        "outcome": outcome,
        "code": code,
    }


def stats(values):
    values = [v for v in values if v is not None]
    if not values:
        return {"n": 0}
    return {
        "n": len(values),
        "median": statistics.median(values),
        "min": min(values),
        "max": max(values),
        "mean": round(statistics.fmean(values), 1),
    }


def main():
    os.makedirs(OUT, exist_ok=True)
    rows = []
    for i in range(REPS):
        # Alternate the arms so a warming cache cannot favour one of them.
        for arm in ("gate-on", "gate-off"):
            row = one(arm)
            row["rep"] = i
            rows.append(row)
            print(json.dumps(row), flush=True)
    report = {
        "reps": REPS,
        "arm_order": "alternating gate-on, gate-off",
        "rows": rows,
        "summary": {
            arm: {
                "wall_ms": stats([r["wall_ms"] for r in rows if r["arm"] == arm]),
                "self_s": stats([r["self_s"] for r in rows if r["arm"] == arm]),
                "pss_peak_kib": stats(
                    [r["pss_peak_kib"] for r in rows if r["arm"] == arm]
                ),
                "failures": [
                    r for r in rows if r["arm"] == arm and r["rc"] != 0
                ],
            }
            for arm in ("gate-on", "gate-off")
        },
    }
    path = os.path.join(OUT, "session-ab.json")
    with open(path, "w") as f:
        json.dump(report, f, indent=2)
    print(json.dumps(report["summary"], indent=2))
    print(f"written: {path}")


if __name__ == "__main__":
    main()
