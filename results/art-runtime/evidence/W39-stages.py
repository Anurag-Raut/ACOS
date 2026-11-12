#!/usr/bin/env python3
"""W39 stage table: turn `acos-art: stage <name> exec=<ms>` markers into
per-launch stage durations and report medians (tools/benchlib.stats).

    W39-stages.py <dir-with-stderr-files> [dir ...]

Each `stderr-*.txt` is one cold launch (the warmup file is skipped). Stages are
the deltas between consecutive *boundary* markers; per-overlay/cache markers
(`*-hit`, `*-built`, `overlay-enter …`, `overlays-list`) are diagnostic only.
Run with `python3 -I` (untrusted-output hygiene); benchlib is imported from the
repository's tools/ by absolute path.
"""
import glob
import json
import os
import re
import sys

ROOT = "/home/Qui/Projects/acos"
sys.path.insert(0, os.path.join(ROOT, "tools"))
import benchlib  # noqa: E402

# Boundary markers in launch order; a duration is marker[i] - marker[i-1].
STAGES = [
    "launch-entry",
    "linkerconfig",
    "classpath",
    "guest-dex",
    "render-shim",
    "art-shim",
    "binder-driver",
    "run-dir",
    "apk-stage",
    "native-libs",
    "overlays",
    "props-logd",
    "peer-setup",
    "guest-exec",
    "guest-spawned",
    "guest-exit",
    "teardown",
]
IGNORE = re.compile(r"^(linkerconfig-(hit|built)|classpath-(hit|built)|overlay-enter .*|overlays-list|overlays-done|ensure-data)$")
STAGE_RE = re.compile(r"^acos-art: stage (\S+) exec=(\d+)ms$")


def parse(path):
    stamps = {}
    for line in open(path, "r", errors="replace"):
        m = STAGE_RE.match(line.strip())
        if not m:
            continue
        name, ms = m.group(1), int(m.group(2))
        if name in STAGES and name not in stamps:
            stamps[name] = ms
        elif not IGNORE.match(name):
            pass
    return stamps


def samples_for(dirpath):
    walls = []
    per_stage = {s: [] for s in STAGES[1:]}
    for sp in sorted(glob.glob(os.path.join(dirpath, "samples.txt"))):
        for line in open(sp):
            tag, rc, ms = line.split()
            if tag == "warmup":
                continue
            walls.append(int(ms))
    for path in sorted(glob.glob(os.path.join(dirpath, "stderr-*.txt"))):
        if os.path.basename(path) == "stderr-warmup.txt":
            continue
        stamps = parse(path)
        prev = None
        for s in STAGES:
            if s not in stamps:
                prev = None if s == "launch-entry" else prev
                continue
            if prev is not None and s != "launch-entry":
                per_stage[s].append(stamps[s] - stamps[prev])
            prev = s
    return walls, per_stage


def row(name, values):
    if not values:
        return None
    st = benchlib.stats(values)
    return {
        "stage": name,
        "n": st["n"],
        "p50": round(st["p50"], 1),
        "p95": round(st["p95"], 1),
        "min": st["min"],
        "max": st["max"],
        "spread_pct": (round(st["spread"] * 100, 1)
                       if st.get("spread") is not None else None),
    }


def main():
    out = {}
    for d in sys.argv[1:]:
        walls, per_stage = samples_for(d)
        st = benchlib.stats(walls)
        rows = [row(s, per_stage[s]) for s in STAGES[1:]]
        rows = [r for r in rows if r]
        out[os.path.basename(d.rstrip("/"))] = {
            "wall": {
                "n": st["n"], "p50": st["p50"], "p95": st["p95"],
                "spread_pct": (round(st["spread"] * 100, 1)
                               if st.get("spread") is not None else None),
                "ci95": [round(st["ci95_low"], 1), round(st["ci95_high"], 1)],
            },
            "stages": rows,
        }
    print(json.dumps(out, indent=2))


if __name__ == "__main__":
    main()
