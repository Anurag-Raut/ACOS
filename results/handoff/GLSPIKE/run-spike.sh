#!/usr/bin/env bash
# GLSPIKE: the acceptance arms. One session per scenario, every ART run through
# the shared lock + guard, private runtime/session dirs.
#
#   results/handoff/GLSPIKE/run-spike.sh off            # default arm (flag unset)
#   GLES=angle results/handoff/GLSPIKE/run-spike.sh on  # software-GLES arm
#
# `off` runs the two scenarios that must stay green with the flag unset
# (a11y-screenshot-wrap asserts the visible skip line); `on` runs the GL arm.
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
cd "$ROOT"
export ACOS_FONT_DIR=/home/Qui/Projects/acos/.cache/boot/android-34/fonts
export ACOS_RUNTIME_DIR="$ROOT/.cache/glspike-run"
export ACOS_SESSION_SOCKET="$ROOT/.cache/glspike-run/session.sock"
export ACOS_VIEW_TEST_DIR="$ROOT/.cache/glspike-lv"
export ACOS_ART_BIN="$ROOT/.cache/glspike-target/release/acos-art"
export ACOS_TRACE_FILE=""
unset ACOS_ART_SYSROOT ACOS_HOME || true
ACOS_CLI="$ROOT/.cache/glspike-target/release/acos"
ARM="${1:?arm: off|on}"
PROVIDER="${GLES:-angle}"

case "$ARM" in
  off)
    unset ACOS_GUEST_GLES
    OFF=(scenarios/a11y-screenshot-wrap.yaml scenarios/a11y-probe.yaml)
    ;;
  on)
    OFF=()
    export ACOS_GUEST_GLES="$PROVIDER"
    export ACOS_GUEST_GLES_TRACE=1
    ;;
  *) echo "arm must be off|on" >&2; exit 2 ;;
esac

run() {
  local scenario="$1" out="$2"
  shift 2
  rm -rf "$out"
  mkdir -p "$(dirname "$out")"
  echo "=== $scenario"
  tools/art-runtime/run-exclusive.sh --task GLSPIKE -- \
    tools/guarded.sh --mem 10G --time 900 -- \
    env -u ACOS_HOME "$ACOS_CLI" verify --engine art --scenario "$scenario" \
      --json --artifacts "$out" "$@" >"$out.log" 2>&1 || true
  python3 - "$out/verdict.json" "$scenario" <<'PY'
import json, sys
path, name = sys.argv[1], sys.argv[2]
try:
    v = json.load(open(path))
except Exception as error:
    print(f"{name}: no verdict ({error})")
    raise SystemExit(0)
steps = v.get("steps", [])
passed = sum(1 for s in steps if s.get("status") == "pass")
print(f"{name}: {v.get('status')} {passed}/{len(steps)} steps")
for s in steps:
    if s.get("status") != "pass":
        print(f"  {s.get('index')} {s.get('step')} {s.get('status')}: "
              f"{(s.get('error') or {}).get('message', '')[:200]}")
PY
}

EVID="$ROOT/results/handoff/GLSPIKE"
if [ "$ARM" = off ]; then
  for scenario in "${OFF[@]}"; do
    run "$scenario" "$EVID/off/$(basename "$scenario" .yaml)"
  done
else
  run scenarios/gles-software.yaml "$EVID/on-${PROVIDER}/gles-software"
fi
