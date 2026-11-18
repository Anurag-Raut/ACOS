#!/usr/bin/env bash
# The release gate's explicit run of the heavy acos-webview tests (HYG4 (b)).
#
#   tools/art-runtime/test-webview-heavy.sh <TASK>
#
# These tests start the real pinned Chromium engine, so they are gated on
# ACOS_WEBVIEW_TEST=1. The default gate excludes them (tools/test-prereq-optouts.txt);
# this script runs exactly those names, with the env set, under the ART lock and the
# guard. A skipped test is a failure here, the same as in test-crates.sh.
# The engine is downloaded once (SHA-pinned) into ACOS_WEBVIEW_CACHE, which defaults
# to the worktree's .cache/webview-engine.
set -uo pipefail
task=${1:?usage: $0 <TASK>}
wt=$PWD
export ACOS_WEBVIEW_TEST=1
export ACOS_WEBVIEW_CACHE=${ACOS_WEBVIEW_CACHE:-$wt/.cache/webview-engine}
mkdir -p "$ACOS_WEBVIEW_CACHE"
log=${ACOS_TEST_LOG:-$wt/.cache/test-logs/$task-$(date -u +%Y%m%dT%H%M%SZ).log}
mkdir -p "$(dirname "$log")"

names=$(grep -v '^#' tools/test-prereq-optouts.txt | awk '/^[a-z_0-9]+ +/ && /ACOS_WEBVIEW_TEST=1/ {print $1}')
[ -n "$names" ] || { echo "FAIL: no heavy webview tests listed in tools/test-prereq-optouts.txt" >&2; exit 2; }

export HEAVY_NAMES="$names"
tools/art-runtime/run-exclusive.sh --task "$task" -- \
  tools/guarded.sh --mem "${ACOS_TEST_MEM:-12G}" --time "${ACOS_TEST_TIME:-1500}" -- \
  bash -c 'rc=0; for n in $HEAVY_NAMES; do
    echo "== $n"
    cargo test --offline -p acos-webview -- --exact --nocapture "$n" || rc=1
  done; exit $rc' 2>&1 | tee "$log"
rc=${PIPESTATUS[0]}
skips=$(grep -c '^SKIP' "$log" 2>/dev/null || true)
ran=$(grep -c '\.\.\. ok$' "$log" 2>/dev/null || true)
echo "HEAVY WEBVIEW: ran_ok=${ran:-0} skip_lines=${skips:-0} rc=$rc (log: $log)"
[ "$rc" -eq 0 ] && [ "${skips:-0}" -eq 0 ] || exit 1
