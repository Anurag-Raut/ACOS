#!/usr/bin/env bash
# GLSPIKE: per-session memory/cost samples. Runs N sessions per arm inside one
# lock acquisition (the lock is released between the two batches so no other
# agent waits on us for more than a few minutes).
#
#   results/handoff/GLSPIKE/run-memory.sh [N]
#
# Arm A (`gl-touched`): scenarios/gles-software.yaml with ACOS_GUEST_GLES=angle —
#        every session reaches libEGL, loads the staged driver and dies at the
#        driver's own EGL_NOT_INITIALIZED (no working GL exists yet).
# Arm B (`gl-unused-off` / `gl-unused-on`): scenarios/counter.yaml never touches
#        EGL; it is the laziness and M8-tap check (flag off vs on).
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
cd "$ROOT"
N="${1:-10}"
export ACOS_FONT_DIR=/home/Qui/Projects/acos/.cache/boot/android-34/fonts
export ACOS_RUNTIME_DIR="$ROOT/.cache/glspike-run"
export ACOS_SESSION_SOCKET="$ROOT/.cache/glspike-run/session.sock"
export ACOS_VIEW_TEST_DIR="$ROOT/.cache/glspike-lv"
export ACOS_ART_BIN="$ROOT/.cache/glspike-target/release/acos-art"
unset ACOS_ART_SYSROOT ACOS_HOME || true
ACOS_CLI="$ROOT/.cache/glspike-target/release/acos"
EVID="$ROOT/results/handoff/GLSPIKE"

# $1 arm, $2 scenario, $3 repetitions
batch() {
  local arm="$1" scenario="$2" reps="$3"
  for i in $(seq 1 "$reps"); do
    local out="$EVID/memory/$arm/$i"
    rm -rf "$out"
    mkdir -p "$(dirname "$out")"
    "$ACOS_CLI" verify --engine art --scenario "$scenario" --json \
      --artifacts "$out" >"$out.log" 2>&1 || true
    python3 - "$out" "$arm" "$i" <<'PY'
import json, os, re, sys, glob
out, arm, i = sys.argv[1], sys.argv[2], sys.argv[3]
verdict = json.load(open(os.path.join(out, "verdict.json"))) \
    if os.path.exists(os.path.join(out, "verdict.json")) else {}
exits = glob.glob(os.path.join(out, "art-frames", "session-exit.json"))
peak = wall = "?"
peak_kb = None
if exits:
    d = json.load(open(exits[0]))
    m = re.search(r"peak=(\d+)MiB", d.get("cgroup") or "")
    peak = m.group(1) if m else "?"
    wall = d.get("wall_ms", "?")
steps = verdict.get("steps", [])
passed = sum(1 for s in steps if s.get("status") == "pass")
line = f"{arm}\t{i}\t{verdict.get('status','?')}\t{passed}/{len(steps)}\t{peak}\t{wall}"
# the guest's own EGL outcome, when it reached EGL at all
log = os.path.join(out, "art-frames", "session-exit.json")
if os.path.exists(log):
    tail = "\n".join(json.load(open(log)).get("logd_tail") or [])
    for needle, tag in (("couldn't find an OpenGL ES", "libEGL_fatal"),
                        ("EGL_NOT_INITIALIZED", "driver_not_initialized"),
                        ("load_driver(", "driver_load_failed"),
                        ("gles_draw start=1", "gles_draw_started")):
        if needle in tail:
            line += f"\t{tag}"
print(line)
PY
  done
}

echo "# arm	rep	status	steps	peak_mib	wall_ms	marker"
batch "${ARM:-gl-touched}" "${SCENARIO:-scenarios/gles-software.yaml}" "$N" </dev/null
