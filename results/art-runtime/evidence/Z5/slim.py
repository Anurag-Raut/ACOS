#!/usr/bin/env python3
"""Copy the Z5 drive.py reports into results/art-runtime/evidence/Z5/<name>.json, dropping the
per-session `steps` blobs (the full view-tree replies) and keeping everything the gate table
rests on: verdicts, failures, ready/timing, CPU, the memory rows and the md5s.

    python3 slim.py /home/Qui/Projects/acos/.cache/z5-run
"""
import json
import os
import sys

KEEP = ("tag", "ready_ms", "t_ready_ms", "count_initial", "count_after_tap",
        "count_after_settle_tap", "isolation_reads", "md5_initial", "md5_after_tap",
        "frame_initial", "frame_after_tap", "fnv_initial", "fnv_after_tap", "tap_ok", "tap_ms",
        "close", "error")


def main():
    src = sys.argv[1] if len(sys.argv) > 1 else "/home/Qui/Projects/acos/.cache/z5-run"
    dst = os.path.join(os.path.dirname(os.path.abspath(__file__)))
    names = sorted(n for n in os.listdir(src) if n.endswith(".json")
                   and n.split("-")[0] in ("a", "b", "c"))
    for name in names:
        d = json.load(open(os.path.join(src, name)))
        slim = {k: v for k, v in d.items() if k != "sessions"}
        slim["sessions"] = {
            k: {kk: vv for kk, vv in s.items() if kk in KEEP}
            for k, s in (d.get("sessions") or {}).items()
        }
        slim["session_count"] = len(d.get("sessions") or {})
        out = os.path.join(dst, name)
        with open(out, "w") as f:
            json.dump(slim, f, indent=1, sort_keys=True)
        print(f"{out}  {os.path.getsize(out) // 1024} KiB")


if __name__ == "__main__":
    main()
