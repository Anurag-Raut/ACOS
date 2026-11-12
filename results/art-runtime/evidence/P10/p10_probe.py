#!/usr/bin/env python3
"""P10: idle probe walk rate + post-tap probe latency, before/after.

One held `acos serve` ART session, counter.apk. The guest's `ACOSPROBE` lines are
teed to a host file with the existing ACOSLAUNCH_GUEST_STDOUT_FILE switch, so
every sample the guest emits (and its `cause`, after P10) is observable from the
host without adding a production trace.

Phases:
  idle   -- after a 3 s settle, count ACOSPROBE lines for N seconds (walks/s and
            cause histogram).
  taps   -- acos_tap (fast) -> wait for the probe line whose tree carries the
            post-tap count -> latency = probe.time - tap publish (guest clock,
            both on the host wall clock) and probe arrival at the harness.

Usage (inside run-exclusive.sh + guarded.sh):
  p10_probe.py --acos <binary> --out FILE --scratch DIR [--reps 20]
"""
import argparse
import json
import shutil
import sys
import time
from collections import Counter
from pathlib import Path

HERE = Path(__file__).resolve().parent
REPO = HERE.parents[3]
sys.path.insert(0, str(REPO / "tools"))
sys.path.insert(0, str(REPO / "tools/art-runtime"))
import benchlib          # noqa: E402
import d2_warm           # noqa: E402

APK = REPO / "fixtures/build/apps/counter.apk"


def now_ms():
    return time.time() * 1000.0


def parse_probe(line):
    line = line.strip()
    if not line.startswith("ACOSPROBE {"):
        return None
    try:
        return json.loads(line[len("ACOSPROBE "):])
    except ValueError:
        return None


def count_of(sample):
    """The counter's text from a raw guest probe (its node field is `id`)."""
    for node in d2_warm.agent_loop.walk((sample.get("tree") or {}).get("nodes")):
        rid = node.get("id") or node.get("resource_id") or ""
        if rid == "count" or rid.endswith(":id/count"):
            return (node.get("text") or "").strip()
    return None


def read_lines(path, offset):
    """Read complete lines appended after `offset`; return (lines, next_offset)."""
    try:
        with open(path, "rb") as f:
            f.seek(offset)
            data = f.read()
    except OSError:
        return [], offset
    if not data:
        return [], offset
    end = data.rfind(b"\n")
    if end < 0:
        return [], offset
    chunk = data[: end + 1]
    return chunk.decode("utf-8", "replace").splitlines(), offset + end + 1


def idle_rate(tee, seconds):
    off = tee.stat().st_size if tee.exists() else 0
    deadline = time.time() + seconds
    lines = []
    while time.time() < deadline:
        new, off = read_lines(tee, off)
        lines.extend(new)
        time.sleep(0.05)
    samples = [s for s in (parse_probe(x) for x in lines) if s]
    causes = Counter(s.get("cause", "none") for s in samples)
    return {
        "seconds": seconds,
        "samples": len(samples),
        "walks_per_s": round(len(samples) / seconds, 2),
        "causes": dict(causes),
        "seq": [samples[0].get("seq"), samples[-1].get("seq")] if samples else None,
    }


def tap_latency(daemon, base, tee, off, reps, expected_start=1):
    rows = []
    for i in range(reps):
        expected = str(expected_start + i)
        t0 = now_ms()
        reply = daemon.call(
            "acos_tap", {"base": base, "id": "inc", "settle": "fast"})
        wall_ms = reply["wall_ms"]
        env = reply.get("envelope") or {}
        found = None
        host_seen = None
        deadline = time.time() + 2.0
        while time.time() < deadline:
            new, off = read_lines(tee, off)
            for line in new:
                sample = parse_probe(line)
                if sample is None:
                    continue
                if count_of(sample) == expected:
                    found = sample
                    host_seen = now_ms()
                    break
            if found:
                break
            time.sleep(0.002)
        rows.append({
            "rep": i + 1,
            "expected": expected,
            "ok": bool(env.get("ok")),
            "tap_wall_ms": round(wall_ms, 3),
            "tap_state_changed": (env.get("result") or {}).get("state_changed"),
            "probe_found": found is not None,
            "probe_seq": (found or {}).get("seq"),
            "probe_cause": (found or {}).get("cause", "none"),
            "probe_latency_ms": (None if found is None
                                 else round(found.get("time", 0) - t0, 3)),
            "probe_arrival_ms": (None if host_seen is None
                                 else round(host_seen - t0, 3)),
            "emit_us": (found or {}).get("emit_us"),
            "count_matches": found is not None,
        })
        # Let the ripple + the dirty probe finish before the next tap.
        time.sleep(0.5)
    return rows, off


def pct(values, p):
    values = sorted(v for v in values if v is not None)
    if not values:
        return None
    if len(values) == 1:
        return values[0]
    k = (len(values) - 1) * p / 100.0
    lo = int(k)
    hi = min(lo + 1, len(values) - 1)
    return values[lo] + (values[hi] - values[lo]) * (k - lo)


def summarize(rows):
    lat = [r["probe_latency_ms"] for r in rows if r["probe_found"]]
    arr = [r["probe_arrival_ms"] for r in rows if r["probe_found"]]
    return {
        "n": len(rows),
        "found": sum(1 for r in rows if r["probe_found"]),
        "latency_p50_ms": None if not lat else round(pct(lat, 50), 3),
        "latency_p95_ms": None if not lat else round(pct(lat, 95), 3),
        "arrival_p50_ms": None if not arr else round(pct(arr, 50), 3),
        "arrival_p95_ms": None if not arr else round(pct(arr, 95), 3),
        "causes": dict(Counter(r["probe_cause"] for r in rows)),
    }


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--acos", required=True)
    ap.add_argument("--out", required=True)
    ap.add_argument("--scratch", required=True)
    ap.add_argument("--reps", type=int, default=20)
    ap.add_argument("--idle-seconds", type=float, default=6.0)
    ap.add_argument("--skip-taps", action="store_true")
    ap.add_argument("--skip-idle", action="store_true")
    args = ap.parse_args()
    scratch = Path(args.scratch)
    if scratch.exists():
        shutil.rmtree(scratch)
    scratch.mkdir(parents=True)
    tee = scratch / "guest-stdout.log"
    # The tee is an existing acos-art diagnostic: it appends the guest's own
    # stdout lines (including every ACOSPROBE sample) to this file.
    import os
    os.environ["ACOSLAUNCH_GUEST_STDOUT_FILE"] = str(tee)
    doc = {"schema": "acos-p10-probe/1", "utc": d2_warm.utc(),
           "apk_sha256": benchlib.sha256_file(APK), "acos": args.acos,
           "host_before": d2_warm.host_now(), "ok": False}
    serve = daemon = None
    try:
        serve = d2_warm.Serve(args.acos, scratch)
        daemon = serve.client()
        daemon.call("ping", {})
        base = {"request_id": "p10-probe", "session_id": "s_p10_probe"}
        launch = daemon.call("acos_launch", {"base": base, "apk": str(APK)})
        if not launch["envelope"].get("ok"):
            raise RuntimeError("launch failed: %s" % (launch["envelope"],))
        doc["launch_wall_ms"] = round(launch["wall_ms"], 3)
        time.sleep(3.0)
        off = tee.stat().st_size
        if not args.skip_idle:
            doc["idle"] = idle_rate(tee, args.idle_seconds)
            off = tee.stat().st_size
        if not args.skip_taps:
            rows, off = tap_latency(daemon, base, tee, off, args.reps)
            doc["taps"] = rows
            doc["taps_summary"] = summarize(rows)
            doc["fresh"] = "%d/%d" % (
                sum(1 for r in rows if r["count_matches"]), len(rows))
        doc["ok"] = True
    except Exception as exc:  # noqa: BLE001
        doc["error"] = "%s: %s" % (type(exc).__name__, exc)
    finally:
        if daemon is not None:
            try:
                daemon.call("acos_destroy", {"base": {
                    "request_id": "p10-destroy", "session_id": "s_p10_probe"}})
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
        print("p10: wrote %s ok=%s" % (args.out, doc.get("ok")))
        shutil.rmtree(scratch, ignore_errors=True)
    return 0 if doc.get("ok") else 1


if __name__ == "__main__":
    sys.exit(main())
