#!/usr/bin/env bash
# GLSPIKE4: run one scenario through the ART engine with private runtime dirs.
#   run-arm.sh <outname> <scenario.yaml> [KEY=VAL ...]   (run from the worktree root,
#   inside tools/art-runtime/run-exclusive.sh + tools/guarded.sh)
set -uo pipefail
ROOT="$PWD"
export ACOS_FONT_DIR=/home/Qui/Projects/acos/.cache/boot/android-34/fonts
out_tag="${1:?}"
export ACOS_RUNTIME_DIR="$ROOT/.cache/g4r-$out_tag"
export ACOS_SESSION_SOCKET="$ROOT/.cache/g4r-$out_tag/session.sock"
export ACOS_VIEW_TEST_DIR="$ROOT/.cache/lv"
export ACOS_ART_BIN="$ROOT/.cache/glspike4-target/release/acos-art"
export ACOS_TRACE_FILE=""
export ACOS_ART_LOG_CAP="${ACOS_ART_LOG_CAP:-200000}"
unset ACOS_ART_SYSROOT ACOS_HOME ACOS_GUEST_GLES || true
out="$1"; scen="$2"; shift 2
for kv in "$@"; do export "$kv"; done
mkdir -p "$ACOS_RUNTIME_DIR" "$ACOS_VIEW_TEST_DIR"
OUT="$ROOT/results/handoff/GLSPIKE4/$out"
rm -rf "$OUT"
"$ROOT/.cache/glspike4-target/release/acos" verify --engine art \
  --scenario "$scen" --json --artifacts "$OUT" > "$OUT.log" 2>&1
echo "rc=$? out=$OUT"
