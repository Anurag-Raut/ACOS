#!/usr/bin/env bash
#
# guarded.sh -- run a command under a hard memory + wall-clock bound.
#
# Every ACOS process that can execute guest code or build it (cargo test, the
# `tools/*.sh` runners, `acos run-dex`, benchmarks) must be started through this
# wrapper (ADR-0018). It enforces, in order of preference:
#
#   1. a systemd user scope with MemoryMax + swap disabled (hard, cgroup-level),
#   2. `ulimit -v` (address-space cap) when systemd is unavailable.
#
# It also runs the command under `timeout`, kills the whole process group on
# exit (normal, error or timeout), exports the VM-side ceilings so the product
# guard matches the wrapper, and removes its scratch directory.
#
# Usage:
#   tools/guard/guarded.sh [--mem 4G] [--vmem 8000000] [--time 60] -- CMD [ARGS...]
#
#   --mem SIZE    systemd MemoryMax (default 4G; accepts K/M/G)
#   --vmem KB     `ulimit -v` fallback in KiB (default 8000000 = ~8 GiB)
#   --time SECS   wall-clock timeout (default 60)
#
# Env overrides: ACOS_GUARD_MEM, ACOS_GUARD_VMEM_KB, ACOS_GUARD_TIME,
# ACOS_GUARD_TASKS_MAX (default 8192; the S3b limit was 512).
#
# Examples:
#   tools/guard/guarded.sh --time 600 -- cargo test -p acos-vm
#   tools/guard/guarded.sh --mem 2G --time 30 -- ./target/release/acos run-dex x.dex
set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
MEM="${ACOS_GUARD_MEM:-4G}"
VMEM_KB="${ACOS_GUARD_VMEM_KB:-8000000}"
TIME="${ACOS_GUARD_TIME:-60}"
# S3b: 512 was too small for a 20-session cohort. Each session needs a
# launcher plus an ART guest (main, GC, JIT, binder, probe and one hosted vsync
# thread), i.e. ~25 tasks; 512 leaves no headroom and a `pthread_create`
# EAGAIN inside the cohort reaches the guest as a Chrooted Choreographer
# failure (exit 1). The pids controller still bounds a fork bomb here.
TASKS_MAX="${ACOS_GUARD_TASKS_MAX:-8192}"
LOG="${ACOS_GUARD_LOG:-$ROOT/results/tmp/guarded.log}"

usage() {
    sed -n '2,32p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
}

while [ $# -gt 0 ]; do
    case "$1" in
        --mem)  MEM="$2"; shift 2 ;;
        --vmem) VMEM_KB="$2"; shift 2 ;;
        --time) TIME="$2"; shift 2 ;;
        --)     shift; break ;;
        -h|--help) usage; exit 0 ;;
        *) break ;;
    esac
done

if [ $# -eq 0 ]; then
    echo "guarded: no command given" >&2
    usage >&2
    exit 2
fi

mkdir -p "$(dirname "$LOG")" "$ROOT/target/tmp"
SCRATCH="$(mktemp -d "$ROOT/target/tmp/guarded.XXXXXX")"

# Match the product-side ceilings to the wrapper so a guest that ignores the
# host limit still aborts with HOST_MEMORY_LIMIT / TIMEOUT.
export ACOS_WALL_MAX_SECS="${ACOS_WALL_MAX_SECS:-$TIME}"
export ACOS_HOST_RSS_MAX="${ACOS_HOST_RSS_MAX:-2G}"

PGID=""
cleanup() {
    local rc=$?
    if [ -n "$PGID" ]; then
        kill -TERM -- "-$PGID" 2>/dev/null || true
        sleep 0.3
        kill -KILL -- "-$PGID" 2>/dev/null || true
    fi
    rm -rf "$SCRATCH"
    # Reap stale guard scratch dirs (older than a day) so target/tmp cannot grow.
    find "$ROOT/target/tmp" -maxdepth 1 -name 'guarded.*' -type d -mtime +1 \
        -exec rm -rf {} + 2>/dev/null || true
    exit "$rc"
}
trap cleanup EXIT INT TERM

started="$(date +%s)"
echo "$(date -Is) guarded start mem=$MEM vmem=${VMEM_KB}K time=${TIME}s cmd=$*" >>"$LOG"

if command -v systemd-run >/dev/null 2>&1 \
    && systemd-run --user --scope -q true >/dev/null 2>&1; then
    setsid systemd-run --user --scope -q \
        -p MemoryMax="$MEM" -p MemorySwapMax=0 -p TasksMax="$TASKS_MAX" \
        -- timeout -k 5 "$TIME" "$@" &
else
    setsid bash -c 'ulimit -v "$1" 2>/dev/null; exec timeout -k 5 "$2" "${@:3}"' \
        _ "$VMEM_KB" "$TIME" "$@" &
fi
PGID=$!

wait "$PGID"
rc=$?
echo "$(date -Is) guarded end rc=$rc elapsed=$(( $(date +%s) - started ))s cmd=$*" >>"$LOG"
exit "$rc"
