#!/usr/bin/env bash
#
# ramguard.sh -- host RAM watchdog for runaway ACOS processes.
#
# ADR-0018: host RAM must never exceed 90% (internal target 85%). This loop
# samples `/proc/meminfo:MemAvailable` and the RSS of every `target/*/acos*`
# process, and kills a process when either:
#
#   * MemAvailable drops below --min-avail-gib (default 9 GiB, i.e. ~85% used
#     on a 62 GiB host) -- kills the largest ACOS processes first, or
#   * its RSS exceeds --max-rss-gib (default 8 GiB).
#
# Kills send SIGTERM to the process and its children, wait briefly, then
# SIGKILL. Every action is appended to results/tmp/ramguard.log.
#
# Usage:
#   tools/guard/ramguard.sh [--once] [--interval SECS] \
#                     [--min-avail-gib G] [--max-rss-gib G] [--dry-run]
#
# Run it in the background during a parallel wave:
#   tools/guard/ramguard.sh >/dev/null 2>&1 &
# Or once from cron / a supervisor with --once.
set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
LOG="${ACOS_RAMGUARD_LOG:-$ROOT/results/tmp/ramguard.log}"
INTERVAL=5
MIN_AVAIL_KIB=$((9 * 1024 * 1024))   # ~85% used on a 62 GiB host
MAX_RSS_KB=$((8 * 1024 * 1024))      # 8 GiB RSS
ONCE=0
DRY=0

usage() { sed -n '2,30p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'; }

while [ $# -gt 0 ]; do
    case "$1" in
        --once)          ONCE=1 ;;
        --dry-run)       DRY=1 ;;
        --interval)      INTERVAL="$2"; shift ;;
        --min-avail-gib) MIN_AVAIL_KIB=$(( $2 * 1024 * 1024 )); shift ;;
        --max-rss-gib)   MAX_RSS_KB=$(( $2 * 1024 * 1024 )); shift ;;
        -h|--help)       usage; exit 0 ;;
        *) echo "ramguard: unknown argument: $1" >&2; usage >&2; exit 2 ;;
    esac
    shift
done

mkdir -p "$(dirname "$LOG")"
log() { echo "$(date -Is) ramguard: $*" | tee -a "$LOG" >&2; }

rss_kb() {
    awk '/VmRSS/{print $2}' "/proc/$1/status" 2>/dev/null
}

kill_tree() {
    local pid="$1" reason="$2" rss="${3:-?}"
    if [ "$DRY" = 1 ]; then
        log "DRY-RUN would kill pid=$pid rss=${rss}KB: $reason"
        return
    fi
    log "killing pid=$pid rss=${rss}KB: $reason"
    # Kill children first, then the process; escalate if it ignores SIGTERM.
    pkill -TERM -P "$pid" 2>/dev/null || true
    kill -TERM "$pid" 2>/dev/null || true
    sleep 1
    pkill -KILL -P "$pid" 2>/dev/null || true
    kill -KILL "$pid" 2>/dev/null || true
}

# ACOS processes under this repo. Match the executable name (`acos`) *and* a
# repo-relative or absolute `target/.../acos` path in the cmdline, so shell
# wrappers that merely mention the binary are not killed.
acos_pids() {
    local pid cmd
    for pid in $(pgrep -x acos 2>/dev/null); do
        [ "$pid" = "$$" ] && continue
        cmd="$(tr '\0' ' ' < "/proc/$pid/cmdline" 2>/dev/null)"
        case "$cmd" in
            *"$ROOT/target/"*|*"target/debug/acos"*|*"target/release/acos"*)
                echo "$pid" ;;
        esac
    done
}

tick() {
    local avail mem_pressure=0
    avail="$(awk '/MemAvailable/{print $2}' /proc/meminfo 2>/dev/null)"
    if [ -n "$avail" ] && [ "$avail" -lt "$MIN_AVAIL_KIB" ]; then
        mem_pressure=1
        log "MemAvailable ${avail}KiB < ${MIN_AVAIL_KIB}KiB (host RAM >= ~85%)"
    fi

    # Collect (rss, pid) for every ACOS process, largest first.
    local rows=""
    local pid rss
    for pid in $(acos_pids); do
        [ "$pid" = "$$" ] && continue
        rss="$(rss_kb "$pid")"
        [ -n "$rss" ] || continue
        rows+="$rss $pid"$'\n'
    done

    if [ -z "$rows" ]; then
        return
    fi
    while read -r rss pid; do
        [ -n "$rss" ] || continue
        if [ "$rss" -gt "$MAX_RSS_KB" ]; then
            kill_tree "$pid" "RSS ${rss}KiB > ${MAX_RSS_KB}KiB" "$rss"
        elif [ "$mem_pressure" = 1 ]; then
            kill_tree "$pid" "memory pressure (MemAvailable ${avail}KiB)" "$rss"
        fi
    done < <(printf '%s' "$rows" | sort -rn)
}

log "started (interval=${INTERVAL}s min_avail=${MIN_AVAIL_KIB}KiB max_rss=${MAX_RSS_KB}KiB log=$LOG)"
while true; do
    tick
    if [ "$ONCE" = 1 ]; then
        break
    fi
    sleep "$INTERVAL"
done
log "stopped"
