#!/usr/bin/env bash
#
# run-emubench.sh -- drive the EMUBENCH stock-emulator baseline (N=1/2/4).
#
# Owner direction 2026-11-18: the stock Android emulator is a tracked baseline
# for every ACOS release gate.  ADR-0015 is relaxed for this benchmark only:
# N=1, 2 and 4 concurrent instances, <= 2048 MiB guest RAM each, only while host
# usage stays under the 90 % ceiling (the harness checks MemAvailable before
# every start and aborts the N if that start would cross 85 %), always shut down
# (the harness stops every instance in a finally + SIGINT/SIGTERM handler, and
# this driver verifies and force-kills leftovers by PID -- never `pkill -f`).
#
# Each phase is its own exclusive lock acquisition (the lock is never held for
# more than ~10 min), each under tools/guarded.sh with a hard MemoryMax.
#
#   tools/art-runtime/emubench/run-emubench.sh                 # all four phases
#   tools/art-runtime/emubench/run-emubench.sh n1-warm n2      # selected phases
#   tools/art-runtime/emubench/run-emubench.sh --smoke         # wiring check, NOT a claim
#   tools/art-runtime/emubench/run-emubench.sh --self-test     # harness unit checks
set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
PY="$ROOT/tools/art-runtime/emubench/emu_bench.py"
EXCL="/home/Qui/Projects/acos/tools/art-runtime/run-exclusive.sh"
GUARD="$ROOT/tools/guarded.sh"
STAMP="$(date -u +%Y%m%dT%H%M%SZ)"
DATE="$(date -u +%Y-%m-%d)"
OUT="$ROOT/.cache/emubench/$STAMP"
EV="$ROOT/results/handoff/EMUBENCH"
JSON="$ROOT/results/bench/emubench-$DATE.json"
SMOKE=0
PHASES=()
for a in "$@"; do
    case "$a" in
        --smoke) SMOKE=1 ;;
        --self-test) exec python3 -I "$PY" --self-test ;;
        -h|--help) sed -n '2,20p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'; exit 0 ;;
        n1-cold|n1-warm|n2|n4) PHASES+=("$a") ;;
        *) echo "run-emubench.sh: unknown argument $a" >&2; exit 2 ;;
    esac
done
[ ${#PHASES[@]} -gt 0 ] || PHASES=(n1-cold n1-warm n2 n4)
[ "$SMOKE" -eq 1 ] && { PHASES=(n1-cold n1-warm n2); JSON=""; }

mkdir -p "$OUT" "$EV/raw" "$EV/logs" "$EV/scripts" "$(dirname "$JSON")"
DRV="$EV/logs/driver-$STAMP.log"
exec > >(tee -a "$DRV") 2>&1

log() { printf '[emubench %s] %s\n' "$(date -u +%H:%M:%SZ)" "$*"; }

qemu_pids() { ps -eo pid,args | awk '/qemu-system-x86_64/ && !/awk/ {print $1}'; }

hostline() { log "host: $(free -m | awk '/^Mem:/ {printf "avail=%s MiB used=%s MiB", $7, $3}') load1=$(cut -d' ' -f1 /proc/loadavg)"; }

leftover_cleanup() {
    local pids; pids="$(qemu_pids)"
    [ -z "$pids" ] && return 0
    log "LEFTOVER qemu after the run: $pids -- shutting down"
    for p in $pids; do
        port=$(tr '\0' ' ' < "/proc/$p/cmdline" 2>/dev/null | sed -n 's/.*-port \([0-9]*\).*/\1/p')
        [ -n "$port" ] && /usr/bin/adb -s "emulator-$port" emu kill >/dev/null 2>&1 || true
    done
    sleep 5
    for p in $(qemu_pids); do kill -TERM "$p" 2>/dev/null || true; done
    sleep 3
    for p in $(qemu_pids); do kill -KILL "$p" 2>/dev/null || true; done
    sleep 1
    pids="$(qemu_pids)"
    if [ -n "$pids" ]; then
        log "FAILED to shut down qemu: $pids"
        return 1
    fi
    log "leftover emulators shut down"
}

preflight() {
    log "=== EMUBENCH preflight (stamp $STAMP) ==="
    hostline
    local avail; avail="$(free -m | awk '/^Mem:/ {print $7}')"
    [ "${avail:-0}" -gt 16384 ] || { log "too little RAM (${avail} MiB), need > 16 GiB"; exit 1; }
    local used_pct; used_pct="$(df --output=pcent / | tail -1 | tr -dc '0-9')"
    [ "${used_pct:-100}" -lt 86 ] || { log "disk ${used_pct}% used (stop at 86%)"; exit 1; }
    local q; q="$(qemu_pids)"
    [ -z "$q" ] || { log "an emulator is already running: $q"; exit 1; }
    log "adb devices: $(/usr/bin/adb devices | sed '1d' | tr '\n' ';') (only emulator-55xx is ever addressed)"
    python3 -I "$PY" --setup-avds || exit 1
}

run_phase() {
    local ph="$1" tmo="$2"
    local extra=()
    [ "$SMOKE" -eq 1 ] && extra+=(--smoke)
    log "=== phase $ph (guarded --mem 24G --time ${tmo}s) ==="
    hostline
    "$EXCL" --task EMUBENCH --exclusive -- \
        "$GUARD" --mem 24G --time "$tmo" -- \
        python3 -I "$PY" --phase "$ph" --out "$OUT/$ph" \
        --json "$OUT/phase-$ph.json" "${extra[@]}"
    local rc=$?
    log "phase $ph rc=$rc"
    hostline
    leftover_cleanup || rc=1
    cp -f "$OUT/phase-$ph.json" "$EV/raw/" 2>/dev/null || true
    return "$rc"
}

preflight
RC=0
declare -A TMO=([n1-cold]=900 [n1-warm]=900 [n2]=900 [n4]=1200)
for ph in "${PHASES[@]}"; do
    run_phase "$ph" "${TMO[$ph]}" || RC=1
done

if [ "$SMOKE" -eq 0 ]; then
    log "=== merge -> $JSON ==="
    python3 -I "$PY" --merge --json "$JSON" "$OUT"/phase-*.json || RC=1
    cp -f "$OUT"/phase-*.json "$EV/raw/" 2>/dev/null || true
    cp -f "$PY" "$ROOT/tools/art-runtime/emubench/run-emubench.sh" "$EV/scripts/" 2>/dev/null || true
fi

log "=== postflight: no emulator may be left running ==="
leftover_cleanup || RC=1
qleft="$(qemu_pids)"
if [ -z "$qleft" ]; then
    log "PASS no qemu-system process is running (pgrep -f qemu-system equivalent empty)"
else
    log "FAIL qemu still running: $qleft"
    RC=1
fi
hostline
log "artifacts: $OUT ; ${JSON:-<smoke, no merged json>} ; log $DRV"
exit "$RC"
