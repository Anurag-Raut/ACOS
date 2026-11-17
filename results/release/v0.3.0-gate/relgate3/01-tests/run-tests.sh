#!/usr/bin/env bash
# RELGATE3 item 1: `cargo test --workspace --release --no-fail-fast` on the v0.3.0
# RC (rel-030 HEAD c7962f71), split per crate group so no ART-lock acquisition
# runs long (same method as RELGATE1). One shared run-exclusive acquisition per
# crate; the pure-Rust crates run guarded only.
set -uo pipefail
WT="/home/Qui/.t3/worktrees/acos/t3-7a2f4ca5-3dab9ee7edba13ab"
cd "$WT" || exit 2
export CARGO_TARGET_DIR="$WT/.cache/rg3-target"
export ACOS_FONT_DIR=/home/Qui/Projects/acos/.cache/boot/android-34/fonts
unset ACOS_BOOT_DIR XDG_RUNTIME_DIR ACOS_HOME ACOS_SESSION_SOCKET
export ACOS_RUNTIME_DIR="$WT/.cache/rt"
export ACOS_ART_BIN="$CARGO_TARGET_DIR/release/acos-art"
OUT="$WT/results/release/v0.3.0-gate/relgate3/01-tests"
mkdir -p "$ACOS_RUNTIME_DIR"

run() { # name, guarded args..., -- crate args
    local name="$1"; shift
    local mem="$1"; shift
    local tmax="$1"; shift
    echo "== $name $(date -u +%FT%TZ) load1=$(cut -d' ' -f1 /proc/loadavg)" | tee -a "$OUT/driver.log"
    tools/art-runtime/run-exclusive.sh --task RELGATE3 -- \
        tools/guarded.sh --mem "$mem" --time "$tmax" -- \
        cargo test --release --no-fail-fast --offline "$@" \
        > "$OUT/$name.log" 2>&1
    local rc=$?
    echo "== $name rc=$rc $(date -u +%FT%TZ)" | tee -a "$OUT/driver.log"
    grep -E "^test result|^error\[|^error:|FAILED|^failures:" "$OUT/$name.log" | tail -20
    return 0
}

run 02-test-groupB        8G 900 -p acos-vm -p acos-runtime -p acos-sandbox
run 03-test-acos-verify   8G 900 -p acos-verify
run 04-test-acos-art      8G 900 -p acos-art
run 05-test-acos-cli      8G 900 -p acos-cli
run 06-test-acos-services 10G 900 -p acos-services
run 07-test-acos-mcp      8G 900 -p acos-mcp
run 08-test-acos-jni      8G 900 -p acos-jni
echo "== TESTS DONE $(date -u +%FT%TZ)" | tee -a "$OUT/driver.log"
