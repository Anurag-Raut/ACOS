#!/usr/bin/env bash
# MERGE040J: run one scenario on ART with private runtime dirs. run-arm.sh <out> <scenario.yaml>
# (from the worktree root, inside run-exclusive.sh + guarded.sh)
set -uo pipefail
ROOT="$PWD"; T="$ROOT/.cache/m40j-target/release"
export ACOS_FONT_DIR=/home/Qui/Projects/acos/.cache/boot/android-34/fonts
export ACOS_RUNTIME_DIR="$ROOT/.cache/m40j-$1" ACOS_SESSION_SOCKET="$ROOT/.cache/m40j-$1/session.sock"
export ACOS_VIEW_TEST_DIR="$ROOT/.cache/lv" ACOS_ART_BIN="$T/acos-art" ACOS_TRACE_FILE="" ACOS_ART_LOG_CAP=400000
unset ACOS_HOME ACOS_GUEST_GLES; export ACOS_ART_SYSROOT=/home/Qui/Projects/acos/.cache/art/sysroot
mkdir -p "$ACOS_RUNTIME_DIR" "$ACOS_VIEW_TEST_DIR"
OUT="$ROOT/results/handoff/MERGE040I/$1"; rm -rf "$OUT"
"$T/acos" verify --engine art --scenario "$2" --json --artifacts "$OUT" > "$OUT.log" 2>&1
echo "$1 rc=$?"
