#!/usr/bin/env bash
# RELGATE4 step 4: run the 20 Gaten rows + gaten-word-lookup into fresh dirs.
#   sweep.sh <list-file> <outdir> <concurrency>
set -uo pipefail
WT="/home/Qui/.t3/worktrees/acos/t3-8432ccd8-1d58d051095d020b"
LIST=$1; OUT=$2; CONC=${3:-2}
BIN_DIR="$WT/.cache/rg5-target/release"
ACOS="$BIN_DIR/acos"
RUNEX="$WT/tools/art-runtime/run-exclusive.sh"
GUARD="$WT/tools/guarded.sh"
export ACOS_FONT_DIR=/home/Qui/Projects/acos/.cache/boot/android-34/fonts
export ACOS_ART_BIN="$BIN_DIR/acos-art"
export ACOS_WEBVIEW_BIN="$BIN_DIR/acos-webview"
export ACOS_WEBVIEW_CACHE="$WT/.cache/webview"
export ACOS_WEBVIEW_SOCKET_DIR="$WT/.cache/wvrun"
export PATH="$BIN_DIR:$PATH"
unset ACOS_BOOT_DIR XDG_RUNTIME_DIR ACOS_HOME TMPDIR
mkdir -p "$OUT" "$WT/.cache/wvrun"
: > "$OUT/rows.jsonl"

run_idx() {
    local idx=$1
    local s; s=$(sed -n "${idx}p" "$LIST")
    [ -n "$s" ] || return 0
    local name; name=$(echo "$s" | sed 's|^scenarios/||; s|/|__|g; s|\.yaml$||')
    local slot=$(( idx % CONC ))
    local rt="$WT/.cache/rg5-gaten-rt/w$slot"
    mkdir -p "$rt"
    rm -rf "$OUT/$name"
    local t0 t1 rc
    t0=$(date +%s)
    ( cd "$WT" && ACOS_RUNTIME_DIR="$rt" ACOS_SESSION_SOCKET="$rt/sock" \
      "$RUNEX" --task RELGATE5-gaten -- "$GUARD" --mem 8G --time 600 -- \
        "$ACOS" verify "$s" --engine art --artifacts "$OUT/$name/artifacts" --json \
        > "$OUT/$name.json" 2> "$OUT/$name.err" )
    rc=$?
    t1=$(date +%s)
    printf '{"scenario": "%s", "name": "%s", "exit": %d, "secs": %d}\n' "$s" "$name" "$rc" "$((t1-t0))" >> "$OUT/rows.jsonl"
    echo "done rc=$rc ${s} ($((t1-t0))s)"
}
export -f run_idx
export LIST OUT ACOS RUNEX GUARD CONC WT BIN_DIR

seq 1 "$(grep -c . "$LIST")" | xargs -P "$CONC" -n 1 bash -c 'run_idx "$0"' 2>&1
echo "SWEEP_DONE"
