#!/usr/bin/env python3
"""W11 acceptance driver: one live ART session, repeated real taps, >=60 frames.

Launch `counter.apk` interactively (`acos-art launch --until activity-resumed`
with `ACOSLAUNCH_INTERACTIVE=1`), then tap the app's own Increment / Reset
controls by their live-tree coordinates and require, for every pair:

* the tap reply is `ok` (a *fresh settled* frame: the launcher only answers after
  the C1 store advanced, a probe sample was taken after the last store, the
  pipeline was quiet for 150 ms and the pixels changed);
* the live view tree shows the expected counter value;
* the C1 store generation advanced.

At the end it requires `min_frames` distinct stored buffer generations for the
session and reports the guest's own gralloc `allocate` count (from the guest
stderr tee with `ACOSART_HW_VERBOSE=1`) — the allocation count is the direct
measure of whether released slots are recycled.

Run it through the ART lock:

    tools/art-runtime/run-exclusive.sh --task W11-frames -- \
      tools/guarded.sh --mem 8G --time 900 -- \
      python3 results/art-runtime/evidence/W11-frames-driver.py --pairs 8

Exit code 0 = every assertion held; 1 = a real observation failed (the JSON says
which).
"""

import argparse
import json
import os
import re
import select
import subprocess
import sys
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parents[3]


def read_line(proc, timeout):
    ready, _, _ = select.select([proc.stdout], [], [], timeout)
    if not ready:
        return None
    line = proc.stdout.readline()
    return line or None


def parse(reply):
    if not reply:
        return None
    try:
        return json.loads(reply)
    except json.JSONDecodeError:
        return None


def walk(nodes):
    """Depth-first over the live-tree nodes (`tree.nodes`, nested `children`)."""
    for node in nodes or []:
        if not isinstance(node, dict):
            continue
        yield node
        yield from walk(node.get("children"))


def counter_text(tree_reply):
    """The counter TextView's texts from a live-tree reply."""
    tree = (parse(tree_reply) or {}).get("tree") or {}
    texts = []
    counter = None
    for node in walk(tree.get("nodes")):
        text = node.get("text")
        if text not in (None, ""):
            texts.append(str(text))
        if isinstance(node.get("id"), str) and node["id"].endswith(":id/count"):
            counter = str(node.get("text"))
    return texts, tree, counter


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--pairs", type=int, default=8, help="increment+reset pairs")
    ap.add_argument("--min-frames", type=int, default=60)
    ap.add_argument("--out", default=".cache/w11-frames")
    ap.add_argument("--json", default="results/art-runtime/evidence/W11-frames.json")
    args = ap.parse_args()

    out = ROOT / args.out
    out.mkdir(parents=True, exist_ok=True)
    guest_err = out / "guest.err"
    launcher_err = out / "launcher.err"
    env = dict(os.environ)
    env.update(
        {
            "ACOSLAUNCH_INTERACTIVE": "1",
            "ACOSLAUNCH_LIVE_PROBE": "1",
            "ACOSLAUNCH_FRAME_WAIT_SECS": "8",
            "ACOS_INPUT_ACK_MS": "1000",
            "ACOSART_HW_VERBOSE": "1",
            "ACOSLAUNCH_GUEST_STDERR_FILE": str(guest_err),
        }
    )
    # ACOS_MAX_ACQUIRED_BUFFERS is deliberately *not* set: the stock-like default
    # (2) is what this driver has to survive.
    env.pop("ACOS_MAX_ACQUIRED_BUFFERS", None)

    cmd = [
        str(ROOT / "target/debug/acos-art"),
        "launch",
        "--apk",
        str(ROOT / "fixtures/build/apps/counter.apk"),
        "--until",
        "activity-resumed",
        "--out",
        str(out),
        "--timeout",
        "300",
    ]
    proc = subprocess.Popen(
        cmd,
        cwd=str(ROOT),
        env=env,
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=open(launcher_err, "wb"),
        text=True,
        bufsize=1,
    )

    result = {
        "task": "W11",
        "what": "repeated real taps, one live ART session, default maxAcquired=2",
        "started_utc": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
        "pairs": args.pairs,
        "min_frames": args.min_frames,
        "steps": [],
        "failures": [],
    }

    def send(obj):
        proc.stdin.write(json.dumps(obj) + "\n")
        proc.stdin.flush()

    ready = parse(read_line(proc, 300))
    result["ready"] = ready
    if not ready or not ready.get("ok"):
        result["failures"].append("launch did not reach a live frame")
        result["verdict"] = "fail"
        return finish(proc, result, args)

    first_generation = ready.get("buffer_generation") or 0
    send({"cmd": "tree"})
    _, _, counter_at_ready = counter_text(read_line(proc, 30))
    result["counter_at_ready"] = counter_at_ready
    texts, tree, counter_at_ready = counter_text(None)
    result["tree_at_ready"] = tree
    result["counter_at_ready"] = counter_at_ready
    value = None
    for pair in range(args.pairs):
        for label, expect in (("increment", "1"), ("reset", "0")):
            send({"cmd": "tree"})
            before = parse(read_line(proc, 30)) or {}
            send({"cmd": "tap", "x": 176 if label == "increment" else 157, "y": 303 if label == "increment" else 429})
            tap = parse(read_line(proc, 40)) or {}
            send({"cmd": "tree"})
            after = parse(read_line(proc, 30)) or {}
            texts, tree, counter = counter_text(json.dumps(after))
            step = {
                "pair": pair,
                "tap": label,
                "ok": bool(tap.get("ok")),
                "code": tap.get("code"),
                "buffer_generation": tap.get("buffer_generation"),
                "frame_number": tap.get("frame_number"),
                "md5": tap.get("md5"),
                "generation_before": before.get("buffer_generation"),
                "counter": counter,
                "texts": [t for t in texts if t in ("0", "1", "2", "3", "4")],
                "tree_gen": after.get("buffer_generation"),
            }
            result["steps"].append(step)
            if not step["ok"]:
                result["failures"].append(f"{label} {pair}: {step['code']}")
            if step["counter"] != expect:
                result["failures"].append(
                    f"{label} {pair}: counter is {step['counter']!r}, expected {expect!r}"
                )
            if before.get("buffer_generation") == after.get("buffer_generation"):
                result["failures"].append(f"{label} {pair}: the C1 store did not advance")
        value = counter

    send({"cmd": "close"})
    close = parse(read_line(proc, 30)) or {}
    result["close"] = close
    final_generation = int(close.get("frame_seq") or 0)
    last_gen = result["steps"][-1].get("buffer_generation") if result["steps"] else 0
    stored = int(last_gen or 0) - int(first_generation or 0)
    result["c1_generations_start"] = first_generation
    result["c1_generations_end"] = last_gen
    result["stored_frames"] = stored
    if stored < args.min_frames:
        result["failures"].append(
            f"only {stored} stored frames, need >= {args.min_frames}"
        )
    result["last_value"] = value
    return finish(proc, result, args)


def finish(proc, result, args):
    rest = []
    while True:
        line = read_line(proc, 5)
        if not line:
            break
        rest.append(line)
    try:
        proc.wait(timeout=10)
    except subprocess.TimeoutExpired:
        proc.kill()
    result["launcher_tail"] = "".join(rest)[:2000]
    # The guest's own gralloc shim proves how many buffers were ever allocated:
    # one allocation per *new* slot, so this is the slot-recycling measure.
    guest = (Path(result.get("guest_err", "")) if result.get("guest_err") else None)
    guest = ROOT / args.out / "guest.err"
    text = guest.read_text(errors="replace") if guest.exists() else ""
    result["allocations"] = text.count("allocate:")
    result["locks"] = text.count("lock: mmap")
    result["guest_stderr_bytes"] = len(text)
    result["finished_utc"] = time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime())
    result["verdict"] = "pass" if not result["failures"] else "fail"
    path = ROOT / args.json
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(result, indent=1) + "\n")
    print(json.dumps({k: v for k, v in result.items() if k != "steps"}, indent=1))
    print(f"steps={len(result['steps'])} failures={result['failures']}")
    return 0 if result["verdict"] == "pass" else 1


if __name__ == "__main__":
    sys.exit(main())
