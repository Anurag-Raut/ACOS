#!/usr/bin/env bash
# GATENWORD: run one scenario through the ART engine with private runtime dirs.
#   run-arm.sh <outname> <scenario.yaml> [KEY=VAL ...]
# (run from the worktree root, inside tools/art-runtime/run-exclusive.sh +
#  tools/guarded.sh)
set -uo pipefail
ROOT="$PWD"
export ACOS_FONT_DIR=/home/Qui/Projects/acos/.cache/boot/android-34/fonts
out_tag="${1:?}"
export ACOS_RUNTIME_DIR="$ROOT/.cache/gwb-$out_tag"
export ACOS_SESSION_SOCKET="$ROOT/.cache/gwb-$out_tag/session.sock"
export ACOS_VIEW_TEST_DIR="$ROOT/.cache/lv"
export ACOS_ART_BIN="$ROOT/.cache/gwb-target/release/acos-art"
export ACOS_TRACE_FILE=""
export ACOS_ART_LOG_CAP="${ACOS_ART_LOG_CAP:-400000}"
unset ACOS_HOME ACOS_GUEST_GLES; export ACOS_ART_SYSROOT=/home/Qui/Projects/acos/.cache/art/sysroot || true
out="$1"; scen="$2"; shift 2
for kv in "$@"; do export "$kv"; done
mkdir -p "$ACOS_RUNTIME_DIR" "$ACOS_VIEW_TEST_DIR"
OUT="$ROOT/results/handoff/GATENWORD/$out"
rm -rf "$OUT"
"$ROOT/.cache/gwb-target/release/acos" verify --engine art \
  --scenario "$scen" --json --artifacts "$OUT" > "$OUT.log" 2>&1
echo "rc=$? out=$OUT"
