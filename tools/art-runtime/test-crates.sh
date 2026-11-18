#!/usr/bin/env bash
# The documented way to run the crate tests that need ART (HYG2; BACKSETTLE
# README §6 is the false-green this replaces).
#
#   tools/art-runtime/test-crates.sh <TASK> [-p <crate>]... [cargo test args]
#
# Two rules, both of which the plain command got wrong:
#
# 1. **Warm data inside the lock.** `run-exclusive.sh` gives every invocation a
#    fresh, empty `ACOS_ART_DATA` (correct: it is scratch), so the ART-booting
#    cases had no cached guest dex and *skipped*. `ACOS_ART_WARM_DATA` is the
#    warm cache and is exported here (the lock also exports it, so a plain
#    `run-exclusive.sh -- cargo test` gets it too).
# 2. **A skipped test is not a pass.** The exit status of `cargo test` alone
#    cannot tell 6 skipped ART cases from 6 that ran, so this wrapper counts the
#    `SKIP` lines and fails when there is one, printing `SKIP COUNT: 0` when the
#    run is clean. `ACOS_ALLOW_ART_SKIP=1` (a counted skip) is honoured for the
#    individual test helper but still fails here: this script is the acceptance
#    command, so it is the place that must show zero.
#
# Environment: ACOS_TEST_MEM (default 26G), ACOS_TEST_TIME (default 2400),
# ACOS_TEST_LOG (default .cache/test-logs/<TASK>-<utc>.log).
set -uo pipefail
task=${1:?usage: $0 <TASK> [-p <crate>]... [cargo test args]}
shift
wt=$PWD
log=${ACOS_TEST_LOG:-$wt/.cache/test-logs/$task-$(date -u +%Y%m%dT%H%M%SZ).log}
mkdir -p "$(dirname "$log")"
mem=${ACOS_TEST_MEM:-26G}
time_limit=${ACOS_TEST_TIME:-2400}

# Unset the opt-out: this command's contract is "no skips".
unset ACOS_ALLOW_ART_SKIP ACOS_ALLOW_TEST_SKIP

tools/art-runtime/run-exclusive.sh --task "$task" -- \
  tools/guarded.sh --mem "$mem" --time "$time_limit" -- \
  bash -c 'export ACOS_ART_WARM_DATA="${ACOS_ART_WARM_DATA:-$PWD/.cache/art/data}"; \
           echo "ACOS_ART_WARM_DATA=$ACOS_ART_WARM_DATA"; \
           exec cargo test --offline "$@"' test "$@" 2>&1 | tee "$log"
rc=${PIPESTATUS[0]}
skips=$(grep -c '^SKIP ' "$log" 2>/dev/null || true)
summaries=$(grep -c '^SKIP SUMMARY: ' "$log" 2>/dev/null || true)
echo "SKIP COUNT: ${skips:-0} (SKIP SUMMARY lines: ${summaries:-0}; log: $log)"
if [ "${skips:-0}" -ne 0 ]; then
  echo "FAIL: ${skips} test(s) skipped — a skipped test is not a pass (HYG2)." >&2
  grep '^SKIP ' "$log" >&2 || true
  exit 1
fi
exit "$rc"
