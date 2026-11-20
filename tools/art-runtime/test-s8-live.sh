#!/usr/bin/env bash
# The release gate's explicit run of the S8 live hostile test (HYG4).
#
#   tools/art-runtime/test-s8-live.sh <TASK>
#
# `hostile_live_run_is_gated_and_recorded_separately` (acos-cli, sandbox_s8) runs the
# real APK under real ART. It is opt-in by ACOS_S8_LIVE_HOSTILE=1 and listed in
# tools/test-prereq-optouts.txt, so the default gate excludes it. This script runs it
# with the env set, under the ART lock and the guard, and fails on any skip line.
set -uo pipefail
task=${1:?usage: $0 <TASK>}
wt=$PWD
export ACOS_S8_LIVE_HOSTILE=1
export ACOS_RUNTIME_DIR=${ACOS_RUNTIME_DIR:-$wt/.cache/rt-$task}
export ACOS_SESSION_SOCKET=${ACOS_SESSION_SOCKET:-$ACOS_RUNTIME_DIR/session.sock}
mkdir -p -m 700 "$ACOS_RUNTIME_DIR"
log=${ACOS_TEST_LOG:-$wt/.cache/test-logs/$task-$(date -u +%Y%m%dT%H%M%SZ).log}
mkdir -p "$(dirname "$log")"

tools/art-runtime/run-exclusive.sh --task "$task" -- \
  tools/guarded.sh --mem "${ACOS_TEST_MEM:-12G}" --time "${ACOS_TEST_TIME:-1200}" -- \
  bash -c 'cargo build --offline -q -p acos-cli --bin acos || exit 1;
           cargo build --offline -q -p acos-art --bin acos-art || exit 1;
           export ACOS_ART_BIN="${CARGO_TARGET_DIR:-target}/debug/acos-art";
           exec cargo test --offline -p acos-cli --test sandbox_s8 -- --exact --nocapture hostile_live_run_is_gated_and_recorded_separately' 2>&1 | tee "$log"
rc=${PIPESTATUS[0]}
skips=$(grep -c '^SKIP\|skipping live hostile' "$log" 2>/dev/null || true)
echo "S8 LIVE: skip_lines=${skips:-0} rc=$rc (log: $log)"
[ "$rc" -eq 0 ] && [ "${skips:-0}" -eq 0 ] || exit 1
