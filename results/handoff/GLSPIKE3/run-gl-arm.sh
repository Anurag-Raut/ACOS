#!/usr/bin/env bash
# GLSPIKE3: one GL arm run with a dense maps watcher, then the flag-off
# counter/a11y-probe reproductions. One lock acquisition, private runtime dirs.
set -uo pipefail
ROOT="$PWD"
export ACOS_FONT_DIR=/home/Qui/Projects/acos/.cache/boot/android-34/fonts
export ACOS_RUNTIME_DIR="$ROOT/.cache/glspike3-run"
export ACOS_SESSION_SOCKET="$ROOT/.cache/glspike3-run/session.sock"
export ACOS_VIEW_TEST_DIR="$ROOT/.cache/lv"
export ACOS_ART_BIN="$ROOT/.cache/glspike3-target/release/acos-art"
export ACOS_TRACE_FILE=""
export ACOS_ART_LOG_CAP="${ACOS_ART_LOG_CAP:-200000}"
unset ACOS_ART_SYSROOT ACOS_HOME || true
OUT="$ROOT/results/handoff/GLSPIKE3"
mkdir -p "$ACOS_RUNTIME_DIR" "$ACOS_VIEW_TEST_DIR" "$OUT"

arm="$1"
case "$arm" in
  gl)
    export ACOS_GUEST_GLES=angle-swiftshader ACOS_GUEST_GLES_TRACE=1
    rm -rf "$OUT/gl-arm"
    python3 .cache/mapswatch.py 60 > "$OUT/maps-watch.txt" 2>&1 &
    watcher=$!
    "$ROOT/.cache/glspike3-target/release/acos" verify --engine art \
      --scenario scenarios/gles-software.yaml --json \
      --artifacts "$OUT/gl-arm" > "$OUT/gl-arm.log" 2>&1
    wait $watcher
    ;;
  off)
    unset ACOS_GUEST_GLES
    rm -rf "$OUT/off-counter" "$OUT/off-a11y-probe"
    "$ROOT/.cache/glspike3-target/release/acos" verify --engine art \
      --scenario scenarios/counter.yaml --json \
      --artifacts "$OUT/off-counter" > "$OUT/off-counter.log" 2>&1
    "$ROOT/.cache/glspike3-target/release/acos" verify --engine art \
      --scenario scenarios/a11y-probe.yaml --json \
      --artifacts "$OUT/off-a11y-probe" > "$OUT/off-a11y-probe.log" 2>&1
    ;;
esac
