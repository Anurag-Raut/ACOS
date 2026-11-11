#!/usr/bin/env bash
# Host-wide ART run lock. Usage: run-exclusive.sh --task NAME -- <command...>
# Serialises every ART guest run across agents/worktrees (flock on one absolute path),
# gives each invocation unique scratch, and logs UTC start/finish + queue wait.
set -euo pipefail
task=adhoc
if [ "${1:-}" = "--task" ]; then task=$2; shift 2; fi
[ "${1:-}" = "--" ] && shift
[ $# -gt 0 ] || { echo "usage: $0 --task NAME -- cmd..." >&2; exit 2; }
root=/home/Qui/Projects/acos
lockdir=$root/.cache/art-locks; mkdir -p "$lockdir"
log=$lockdir/runs.log
t0=$(date +%s); u0=$(date -u +%FT%TZ)
exec 9>"$lockdir/art.lock"
flock 9
t1=$(date +%s)
export ACOS_ART_DATA="$lockdir/data-$task-$$"; mkdir -p "$ACOS_ART_DATA"
rc=0; "$@" || rc=$?
t2=$(date +%s)
echo "task=$task started_utc=$u0 finished_utc=$(date -u +%FT%TZ) elapsed_s=$((t2-t0)) queue_wait_s=$((t1-t0)) rc=$rc cmd=$*" >> "$log"
rm -rf "$ACOS_ART_DATA"
exit $rc
