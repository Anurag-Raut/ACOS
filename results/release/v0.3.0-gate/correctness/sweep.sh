#!/usr/bin/env bash
# RELGATE1 correctness sweep: run every scenario in a list through `acos verify --engine art`.
#   sweep.sh <list-file> <outdir> <concurrency>
# Each run: own artifacts dir, own private runtime dir/socket, one shared ART slot
# (run-exclusive.sh + guarded.sh). Writes <outdir>/<name>.json, .err and rows.jsonl.
set -uo pipefail
WT=/home/Qui/.t3/worktrees/acos/t3-9fde79f7-5b90a224f291fea1
LIST=$1; OUT=$2; CONC=${3:-3}
ACOS=$WT/.cache/rg1-target/release/acos
RUNEX=/home/Qui/Projects/acos/tools/art-runtime/run-exclusive.sh
GUARD=/home/Qui/Projects/acos/tools/guarded.sh
export ACOS_FONT_DIR=/home/Qui/Projects/acos/.cache/boot/android-34/fonts
unset ACOS_BOOT_DIR XDG_RUNTIME_DIR ACOS_HOME
mkdir -p "$OUT"

run_idx() {
    local idx=$1
    local s; s=$(sed -n "${idx}p" "$LIST")
    [ -n "$s" ] || return 0
    local name; name=$(echo "$s" | sed 's|^scenarios/||; s|/|__|g; s|\.yaml$||')
    local slot=$(( idx % CONC ))
    local rt="$WT/.cache/rg1/rt/w$slot"
    mkdir -p "$rt"
    local t0 t1 rc
    t0=$(date +%s)
    ( cd "$WT" && ACOS_RUNTIME_DIR="$rt" ACOS_SESSION_SOCKET="$rt/sock" \
      "$RUNEX" --task RELGATE1 -- "$GUARD" --mem 6G --time 600 -- \
        "$ACOS" verify "$s" --engine art --artifacts "$OUT/$name/artifacts" --json \
        > "$OUT/$name.json" 2> "$OUT/$name.err" )
    rc=$?
    t1=$(date +%s)
    printf '{"scenario": "%s", "name": "%s", "exit": %d, "secs": %d}\n' "$s" "$name" "$rc" "$((t1-t0))" >> "$OUT/rows.jsonl"
    echo "done rc=$rc ${s} ($((t1-t0))s)"
}
export -f run_idx
export LIST OUT ACOS RUNEX GUARD CONC WT

seq 1 "$(grep -c . "$LIST")" | xargs -P "$CONC" -n 1 bash -c 'run_idx "$0"' 2>&1
echo "SWEEP_DONE"
