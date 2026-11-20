#!/usr/bin/env bash
# GLSPIKE2 cost samples: one session per arm, N repetitions, one lock
# acquisition per run (the shared lock is released between sessions).
#
#   results/handoff/GLSPIKE2/run-memory.sh <arm> <N>
#
#   gl-touched        scenarios/gles-software.yaml with ACOS_GUEST_GLES=angle-swiftshader
#                     (the guest reaches ANGLE; eglInitialize fails, so this is the
#                     cost of the staged provider being *used*, not of a frame)
#   gl-unused-on      scenarios/counter.yaml with the flag on (never touches EGL)
#   gl-unused-off     scenarios/counter.yaml, flag unset (the default guest)
#
# Each sample is the run's own cgroup peak (memory.peak) plus the wall time from
# the session's own exit record; nothing is inferred host-side.
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
cd "$ROOT"
ARM="${1:?arm: gl-touched|gl-unused-on|gl-unused-off}"
N="${2:-1}"
export ACOS_FONT_DIR=/home/Qui/Projects/acos/.cache/boot/android-34/fonts
export ACOS_RUNTIME_DIR="$ROOT/.cache/glspike2-run"
export ACOS_SESSION_SOCKET="$ROOT/.cache/glspike2-run/session.sock"
export ACOS_VIEW_TEST_DIR="$ROOT/.cache/glspike2-lv"
export ACOS_ART_BIN="$ROOT/.cache/glspike2-target/release/acos-art"
export ACOS_TRACE_FILE=""
unset ACOS_ART_SYSROOT ACOS_HOME || true
ACOS_CLI="$ROOT/.cache/glspike2-target/release/acos"
EVID="$ROOT/results/handoff/GLSPIKE2/memory"
case "$ARM" in
  gl-touched)    SCENARIO=scenarios/gles-software.yaml; export ACOS_GUEST_GLES=angle-swiftshader ;;
  gl-unused-on)  SCENARIO=scenarios/counter.yaml;       export ACOS_GUEST_GLES=angle-swiftshader ;;
  gl-unused-off) SCENARIO=scenarios/counter.yaml;       unset ACOS_GUEST_GLES ;;
  *) echo "arm must be gl-touched|gl-unused-on|gl-unused-off" >&2; exit 2 ;;
esac
for i in $(seq 1 "$N"); do
  out="$EVID/$ARM/$i"
  rm -rf "$out"
  mkdir -p "$out"
  tools/art-runtime/run-exclusive.sh --task GLSPIKE2 -- \
    tools/guarded.sh --mem 10G --time 600 -- \
    env -u ACOS_HOME "$ACOS_CLI" verify --engine art --scenario "$SCENARIO" \
      --json --artifacts "$out" >"$out.log" 2>&1 || true
  python3 - "$out" "$ARM" "$i" <<'PY'
import glob, json, os, re, sys
out, arm, i = sys.argv[1], sys.argv[2], sys.argv[3]
verdict = json.load(open(os.path.join(out, "verdict.json"))) \
    if os.path.exists(os.path.join(out, "verdict.json")) else {}
exits = glob.glob(os.path.join(out, "art-frames", "session-exit.json"))
peak = wall = "?"
egl = "none"
if exits:
    d = json.load(open(exits[0]))
    m = re.search(r"peak=(\d+)MiB", d.get("cgroup") or "")
    peak = m.group(1) if m else "?"
    wall = d.get("wall_ms", "?")
    tail = "\n".join(d.get("logd_tail") or [])
    if "EGL_NOT_INITIALIZED" in tail:
        egl = "driver_not_initialized"
    elif "gles_draw start=1" in tail:
        egl = "driver_loaded_no_frame"
    elif "couldn't find an OpenGL ES" in tail:
        egl = "libEGL_fatal"
steps = verdict.get("steps", [])
passed = sum(1 for s in steps if s.get("status") == "pass")
print(f"{arm}\t{i}\t{verdict.get('status','?')}\t{passed}/{len(steps)}\t{peak}MiB\t{wall}ms\t{egl}")
PY
done
