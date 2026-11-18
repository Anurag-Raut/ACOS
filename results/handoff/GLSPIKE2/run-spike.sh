#!/usr/bin/env bash
# GLSPIKE2: the acceptance arms. One session per scenario, every ART run through
# the shared lock + guard, private runtime/session dirs, private sysroot copy
# only when an arm wants it (default: the shared sysroot provider).
#
#   results/handoff/GLSPIKE2/run-spike.sh off                  # default arm
#   GLES=angle-swiftshader results/handoff/GLSPIKE2/run-spike.sh on
#   GLES=angle  results/handoff/GLSPIKE2/run-spike.sh on        # GLSPIKE's arm
#   GLES=angle-swiftshader results/handoff/GLSPIKE2/run-spike.sh gaten
#
# `off` runs the scenarios that must stay green with the flag unset;
# `on` runs the GL arm (gles-software.yaml); `gaten` runs the bubble overlay.
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
cd "$ROOT"
export ACOS_FONT_DIR=/home/Qui/Projects/acos/.cache/boot/android-34/fonts
export ACOS_RUNTIME_DIR="$ROOT/.cache/glspike2-run"
export ACOS_SESSION_SOCKET="$ROOT/.cache/glspike2-run/session.sock"
export ACOS_VIEW_TEST_DIR="$ROOT/.cache/glspike2-lv"
export ACOS_ART_BIN="$ROOT/.cache/glspike2-target/release/acos-art"
export ACOS_TRACE_FILE=""
unset ACOS_ART_SYSROOT ACOS_HOME || true
ACOS_CLI="$ROOT/.cache/glspike2-target/release/acos"
ARM="${1:?arm: off|on|gaten}"
PROVIDER="${GLES:-angle-swiftshader}"

case "$ARM" in
  off)
    unset ACOS_GUEST_GLES
    ARM_DIR=off
    SCENARIOS=(scenarios/a11y-screenshot-wrap.yaml scenarios/a11y-probe.yaml)
    ;;
  on)
    ARM_DIR="on-$PROVIDER"
    SCENARIOS=(scenarios/gles-software.yaml)
    export ACOS_GUEST_GLES="$PROVIDER"
    export ACOS_GUEST_GLES_TRACE=1
    ;;
  gaten)
    ARM_DIR="gaten-$PROVIDER"
    SCENARIOS=(scenarios/gaten-bubble-overlay.yaml)
    export ACOS_GUEST_GLES="$PROVIDER"
    export ACOS_GUEST_GLES_TRACE=1
    ;;
  *) echo "arm must be off|on|gaten" >&2; exit 2 ;;
esac

run() {
  local scenario="$1" out="$2"
  rm -rf "$out"
  mkdir -p "$(dirname "$out")"
  echo "=== $scenario -> $out"
  tools/art-runtime/run-exclusive.sh --task GLSPIKE2 -- \
    tools/guarded.sh --mem 10G --time 900 -- \
    env -u ACOS_HOME "$ACOS_CLI" verify --engine art --scenario "$scenario" \
      --json --artifacts "$out" >"$out.log" 2>&1 || true
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
              f"{str(s.get('detail') or s.get('reason') or '')[:300]}")
PY
}

for scenario in "${SCENARIOS[@]}"; do
  base="$(basename "$scenario" .yaml)"
  run "$scenario" "$ROOT/results/handoff/GLSPIKE2/$ARM_DIR/$base"
done
