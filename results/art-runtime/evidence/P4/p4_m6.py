#!/usr/bin/env python3
"""P4 M6 harness: reset -> first correct (verified) screen, phase split.

Reuses the existing d2_warm/agent_loop transport and the env-gated P3
(`art.tree` settle/capture) and D-M6 (`stop`, `reset`) traces. One held
`acos serve` ART session, counter.apk. Each sample: tap inc -> observe_tree
(before text must be "1") -> reset (timed) -> observe_tree (after text "0",
newer frame) and the reset+verify total is the M6 end-to-end number.

Usage: p4_m6.py --tag before --acos <acos> --art <acos-art> [--resets 21]
"""
import argparse
import json
import os
import statistics
import subprocess
import sys
import time
from pathlib import Path

WORKTREE = Path("/home/Qui/.paseo/worktrees/3oxa6xao/perf-p4-reset")
SCRATCH_ROOT = Path("/home/Qui/Projects/acos/.cache/p4-reset")
sys.path.insert(0, str(WORKTREE / "tools"))
sys.path.insert(0, str(WORKTREE / "tools/art-runtime"))

import d2_warm  # noqa: E402
import agent_loop  # noqa: E402

d2_warm.REPO = WORKTREE
APK = WORKTREE / "fixtures/build/apps/counter.apk"


def walk(nodes):
    for node in nodes or []:
        if not isinstance(node, dict):
            continue
        yield node
        yield from walk(node.get("children"))


def count_text(tree):
    for node in walk((tree or {}).get("nodes")):
        rid = node.get("resource_id") or ""
        if rid == "count" or rid.endswith(":id/count"):
            return d2_warm.count_text(tree)
    return d2_warm.count_text(tree)


def read_new(path, offset):
    try:
        with open(path, "r", errors="replace") as f:
            f.seek(offset)
            chunk = f.read()
            return chunk, f.tell()
    except FileNotFoundError:
        return "", offset


def parse_events(chunk):
    out = {"tree": [], "stop": [], "reset": [], "art-stop": [], "art-evidence": [],
           "art-exit-phases": []}
    for line in chunk.splitlines():
        line = line.strip()
        if not line:
            continue
        if line.startswith("P3TRACE") and "ev=art.tree" in line:
            out["tree"].append(line)
        elif line.startswith("{"):
            try:
                ev = json.loads(line)
            except json.JSONDecodeError:
                continue
            if ev.get("event") in ("stop", "reset", "art-stop", "art-evidence",
                                   "art-exit-phases"):
                out[ev["event"]].append(ev)
    return out


def p3_detail(line):
    fields = {}
    for part in line.split():
        if "=" in part:
            k, v = part.split("=", 1)
            fields[k] = v
    return fields


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--tag", required=True)
    ap.add_argument("--acos", required=True)
    ap.add_argument("--art", required=True)
    ap.add_argument("--resets", type=int, default=21)
    ap.add_argument("--out", default=None)
    args = ap.parse_args()

    scratch = SCRATCH_ROOT / ("run-" + args.tag)
    if scratch.exists():
        import shutil
        shutil.rmtree(scratch)
    (scratch / "screens").mkdir(parents=True)
    p3_path = str(scratch / "p3.trace")
    dm6_path = str(scratch / "dm6.trace")
    os.environ["P3_TRACE"] = p3_path
    os.environ["DM6_RESET_TRACE"] = dm6_path
    os.environ["ACOS_ART_BIN"] = args.art
    os.environ.setdefault("ACOS_FONT_DIR",
                          "/home/Qui/Projects/acos/.cache/boot/android-34/fonts")
    os.environ.pop("ACOS_BOOT_DIR", None)

    serve = d2_warm.Serve(args.acos, scratch, engine="art", max_sessions=1)
    daemon = serve.client()
    base = {"request_id": "p4-%s" % args.tag, "session_id": "s_p4_%s" % args.tag}
    rows = []
    try:
        launch = daemon.call("acos_launch", dict(base, apk=str(APK)))
        if not launch["envelope"].get("ok"):
            raise SystemExit("launch failed: %s" % launch["envelope"])
        # one untimed warm-up reset
        for i in range(-1, args.resets):
            setup = {}
            tap = daemon.call("acos_tap", dict(base, id="inc"))
            setup["tap_ok"] = bool(tap["envelope"].get("ok"))
            before = daemon.call("acos_observe_tree", dict(base, max_nodes=2000))
            setup["before_text"] = count_text((before["envelope"].get("result") or {})
                                              .get("tree"))
            p3_off = os.path.getsize(p3_path) if os.path.exists(p3_path) else 0
            dm6_off = os.path.getsize(dm6_path) if os.path.exists(dm6_path) else 0
            r = daemon.call("acos_reset", dict(base))
            wall = r["wall_ms"]
            chunk, _ = read_new(p3_path, p3_off)
            dchunk, _ = read_new(dm6_path, dm6_off)
            events = parse_events(chunk + "\n" + dchunk)
            verify = daemon.call("acos_observe_tree", dict(base, max_nodes=2000))
            vs = verify["envelope"].get("result") or {}
            vtree = vs.get("tree")
            setup["verify_text"] = count_text(vtree)
            setup["verify_frame_seq"] = (vs.get("frame") or {}).get("frame_seq") if isinstance(
                vs.get("frame"), dict) else None
            value = r["envelope"].get("result") or {}
            seq, prev = value.get("frame_seq"), value.get("previous_frame_seq")
            trees = [p3_detail(line) for line in events["tree"]]
            stop = events["stop"][0] if events["stop"] else {}
            rst = events["reset"][0] if events["reset"] else {}
            row = {
                "i": i,
                "ms": round(wall, 3),
                "verify_ms": round(verify["wall_ms"], 3),
                "total_ms": round(wall + verify["wall_ms"], 3),
                "ok": bool(r["envelope"].get("ok")),
                "state_reset": bool(value.get("state_reset")),
                "data_cleared": bool(value.get("data_cleared")),
                "fresh": isinstance(seq, int) and isinstance(prev, int) and seq > prev,
                "setup": setup,
                "stop_ms": stop.get("stop_ms"),
                "stop_exit_ms": stop.get("exit_ms"),
                "reset_trace": rst,
                "trees": trees,
                "art_stop": events["art-stop"][0] if events["art-stop"] else {},
                "art_evidence": events["art-evidence"][0] if events["art-evidence"] else {},
                "art_exit": events["art-exit-phases"][0] if events["art-exit-phases"] else {},
            }
            row["good"] = bool(
                row["ok"] and row["state_reset"] and row["data_cleared"]
                and row["fresh"] and setup["before_text"] not in (None, "0")
                and setup["verify_text"] == "0"
                and setup["tap_ok"])
            rows.append(row)
    finally:
        try:
            daemon.call("acos_destroy", {"base": base})
        except Exception:
            pass
        daemon.close()
        rc = serve.close()
        time.sleep(0.3)
        surv = d2_warm.survivors(scratch)
    measured = [r for r in rows if r["i"] >= 0]
    good = [r for r in measured if r["good"]]

    def stat(key, rows_):
        vals = [r[key] for r in rows_ if isinstance(r.get(key), (int, float))]
        if not vals:
            return None
        vals = sorted(vals)
        def pct(p):
            k = (len(vals) - 1) * p
            lo, hi = int(k), min(int(k) + 1, len(vals) - 1)
            return round(vals[lo] + (vals[hi] - vals[lo]) * (k - lo), 3)
        return {"n": len(vals), "p50": pct(0.5), "p95": pct(0.95),
                "min": vals[0], "max": vals[-1]}

    def phase_avg(key):
        vals = []
        for r in good:
            tr = r.get("reset_trace") or {}
            v = tr.get(key)
            if isinstance(v, (int, float)):
                vals.append(v)
        return round(statistics.median(vals), 3) if vals else None

    tree_settle = []
    tree_capture = []
    for r in good:
        if len(r["trees"]) >= 1:
            t = r["trees"][0]
            tree_settle.append(int(t.get("settle_us", 0)) / 1000.0)
            tree_capture.append(int(t.get("capture_us", 0)) / 1000.0)
    doc = {
        "tag": args.tag,
        "n_measured": len(measured),
        "n_good": len(good),
        "false_passes": len(measured) - len(good),
        "serve_rc": rc,
        "survivors": surv,
        "host_load": os.getloadavg(),
        "binaries": {
            "acos_sha256": subprocess.run(["sha256sum", args.acos], capture_output=True,
                                          text=True).stdout.split()[0],
            "acos_art_sha256": subprocess.run(["sha256sum", args.art], capture_output=True,
                                              text=True).stdout.split()[0],
        },
        "m6_reset": stat("ms", good),
        "m6_reset_then_verified_screen": stat("total_ms", good),
        "verify_only": stat("verify_ms", good),
        "phases": {
            "stop_ms": stat("stop_ms", good),
            "stop_exit_ms": stat("stop_exit_ms", good),
            "relaunch_ms": phase_avg("relaunch_ms"),
            "bind_ms": phase_avg("bind_ms"),
            "engine_total_ms": phase_avg("engine_total_ms"),
            "bind_settle_ms": stat("x", [{"x": v} for v in tree_settle]),
            "launcher_signal_us": stat("x", [{"x": (r.get("art_stop") or {}).get("signal_us", 0) / 1000.0} for r in good]),
            "launcher_sample_us": stat("x", [{"x": (r.get("art_stop") or {}).get("sample_us", 0) / 1000.0} for r in good]),
            "launcher_cgroup_kill_us": stat("x", [{"x": (r.get("art_stop") or {}).get("cgroup_kill_us", 0) / 1000.0} for r in good]),
            "launcher_wait_us": stat("x", [{"x": (r.get("art_stop") or {}).get("wait_us", 0) / 1000.0} for r in good]),
            "launcher_total_ms": stat("x", [{"x": (r.get("art_stop") or {}).get("total_us", 0) / 1000.0} for r in good]),
            "launcher_evidence_ms": stat("x", [{"x": (r.get("art_evidence") or {}).get("evidence_us", 0) / 1000.0} for r in good]),
            "launcher_exit": {k: stat("x", [{"x": (r.get("art_exit") or {}).get(k, 0)} for r in good])
                              for k in ("live_ms", "streams_ms", "logd_ms", "peer_ms", "checks_ms", "total_ms")},
            "bind_capture_ms": stat("x", [{"x": v} for v in tree_capture]),
        },
        "rows": rows,
    }
    out = Path(args.out) if args.out else (SCRATCH_ROOT / ("m6-%s.json" % args.tag))
    out.write_text(json.dumps(doc, indent=2))
    print(json.dumps({k: doc[k] for k in (
        "tag", "n_measured", "n_good", "false_passes", "m6_reset",
        "m6_reset_then_verified_screen", "verify_only", "phases",
        "host_load", "serve_rc")}, indent=2))
    print("wrote", out)


if __name__ == "__main__":
    main()
