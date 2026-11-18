#!/usr/bin/env bash
# GLSPIKE2: is the staged SwiftShader ICD ever opened by the guest?
#
# Runs the GL arm once and samples /proc/<guest pid>/maps every 100 ms for the
# session's lifetime, recording which of the provider's files (and the image's
# libEGL/libvulkan) are mapped. /proc/<pid>/maps is the guest's own view, so a
# file listed there was really opened by the guest's linker.
#
#   results/handoff/GLSPIKE2/maps-sample.sh [outfile]
set -uo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
cd "$ROOT"
OUT="${1:-$ROOT/results/handoff/GLSPIKE2/maps-sample.txt}"
export ACOS_FONT_DIR=/home/Qui/Projects/acos/.cache/boot/android-34/fonts
export ACOS_RUNTIME_DIR="$ROOT/.cache/glspike2-run"
export ACOS_SESSION_SOCKET="$ROOT/.cache/glspike2-run/session.sock"
export ACOS_VIEW_TEST_DIR="$ROOT/.cache/glspike2-lv"
export ACOS_ART_BIN="$ROOT/.cache/glspike2-target/release/acos-art"
export ACOS_GUEST_GLES=angle-swiftshader
export ACOS_TRACE_FILE=""
unset ACOS_ART_SYSROOT ACOS_HOME || true
: > "$OUT"
tools/art-runtime/run-exclusive.sh --task GLSPIKE2 -- \
  tools/guarded.sh --mem 8G --time 300 -- \
  env -u ACOS_HOME "$ROOT/.cache/glspike2-target/release/acos" verify --engine art \
    --scenario scenarios/gles-software.yaml --json --artifacts .cache/probe/maps-run \
    >/dev/null 2>&1 &
sleep 0.3
for _ in $(seq 1 120); do
  for pid in $(ps -eo pid,args | awk '/dalvikvm64/ && !/awk/ {print $1}'); do
    grep -hoE '/(vendor/lib64/(hw|egl)/[^ ]+|system/lib64/(libEGL|libvulkan|libGLESv2)\.so)' \
      /proc/$pid/maps 2>/dev/null
  done
  sleep 0.04
done | sort | uniq -c | sort -rn >> "$OUT"
wait
echo "--- sampled maps ($OUT):" | tee -a "$OUT"
cat "$OUT"
