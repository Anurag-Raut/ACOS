#!/usr/bin/env bash
# RELGATE4 step 1: `cargo test --workspace --release --no-fail-fast` on RC (afad66858 + relnotes2h),
# split per crate group so no ART-lock acquisition runs long (same method as RELGATE3).
set -uo pipefail
WT="/home/Qui/.t3/worktrees/acos/t3-f4bcc4d4-5c49b0ef4f83b639"
cd "$WT" || exit 2
source "$WT/.cache/env.sh"
export CARGO_TARGET_DIR="$WT/.cache/rg4-target"
unset ACOS_BOOT_DIR XDG_RUNTIME_DIR ACOS_HOME ACOS_ALLOW_ART_SKIP ACOS_ALLOW_TEST_SKIP
export ACOS_RUNTIME_DIR="$WT/.cache/rt"
export ACOS_SESSION_SOCKET="$WT/.cache/rt/session.sock"
export ACOS_ART_BIN="$CARGO_TARGET_DIR/release/acos-art"
export ACOS_WEBVIEW_BIN="$CARGO_TARGET_DIR/release/acos-webview"
export ACOS_WEBVIEW_CACHE="$WT/.cache/webview"
export ACOS_ART_WARM_DATA="$WT/.cache/art/data"
export ACOS_SNAPSHOT_FULL_REAL=1
OUT="$WT/results/release/v0.4.0-gate/relgate4/01-tests"
mkdir -p "$ACOS_RUNTIME_DIR" "$OUT"
: > "$OUT/driver.log"

run_guarded() {
    local name="$1"; shift
    local mem="$1"; shift
    local tmax="$1"; shift
    echo "== $name $(date -u +%FT%TZ) load1=$(cut -d' ' -f1 /proc/loadavg)" | tee -a "$OUT/driver.log"
    tools/guarded.sh --mem "$mem" --time "$tmax" -- \
        cargo test --release --no-fail-fast --offline "$@" \
        > "$OUT/$name.log" 2>&1
    local rc=$?
    echo "== $name rc=$rc $(date -u +%FT%TZ)" | tee -a "$OUT/driver.log"
    grep -E "^test result|^error\[|^error:|FAILED|^failures:" "$OUT/$name.log" | tail -20 | tee -a "$OUT/driver.log"
    return 0
}

run_art() {
    local name="$1"; shift
    local mem="$1"; shift
    local tmax="$1"; shift
    echo "== $name $(date -u +%FT%TZ) load1=$(cut -d' ' -f1 /proc/loadavg)" | tee -a "$OUT/driver.log"
    tools/art-runtime/run-exclusive.sh --task RELGATE4 -- \
        tools/guarded.sh --mem "$mem" --time "$tmax" -- \
        cargo test --release --no-fail-fast --offline "$@" \
        > "$OUT/$name.log" 2>&1
    local rc=$?
    echo "== $name rc=$rc $(date -u +%FT%TZ)" | tee -a "$OUT/driver.log"
    grep -E "^test result|^error\[|^error:|FAILED|^failures:" "$OUT/$name.log" | tail -20 | tee -a "$OUT/driver.log"
    return 0
}

run_guarded 02-test-groupA 12G 900 \
    -p acos-aidl -p acos-apk -p acos-arm64 -p acos-binder-peer \
    -p acos-dex -p acos-framework-natives -p acos-gles -p acos-gles-provider \
    -p acos-native-loader -p acos-netd -p acos-netd-ctl -p acos-parcel \
    -p acos-protocol -p acos-render -p acos-seccomp-guard -p acos-sfgeom \
    -p acos-sqlite -p acos-test-support -p acos-wm

run_art 03-test-groupB        12G 900 -p acos-vm -p acos-runtime -p acos-sandbox
run_art 04-test-acos-verify   12G 900 -p acos-verify
run_art 05-test-acos-art      14G 1200 -p acos-art
run_art 06-test-acos-cli      14G 1200 -p acos-cli
run_art 07-test-acos-services 14G 1200 -p acos-services
run_art 08-test-acos-mcp      12G 900 -p acos-mcp
run_art 09-test-acos-jni      12G 900 -p acos-jni
run_art 10-test-acos-webview  12G 900 -p acos-webview

echo "== TESTS DONE $(date -u +%FT%TZ)" | tee -a "$OUT/driver.log"
df -h / | tail -1 | tee -a "$OUT/driver.log"
