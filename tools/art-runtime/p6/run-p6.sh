#!/usr/bin/env bash
#
# run-p6.sh — drive the P6 stock-emulator baseline (M1-M8 + CPU).
#
# Holds the host-wide ART measurement lock for the whole emulator run
# (run-exclusive.sh) so the emulator's CPU load cannot skew other agents'
# measurements, and runs the harness under guarded.sh (hard MemoryMax + wall
# clock).  ADR-0015 rules are enforced before the emulator starts and verified
# again after the run: <= 2 GiB guest RAM, exactly ONE instance, always shut
# down (no qemu-system process may survive).
#
#   tools/art-runtime/p6/run-p6.sh                 # full measurement run
#   tools/art-runtime/p6/run-p6.sh --smoke         # 1-3 samples per row (NOT a claim)
#   tools/art-runtime/p6/run-p6.sh --self-test     # harness unit checks, no emulator
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
PY="$ROOT/tools/art-runtime/p6/p6_emulator.py"
STAMP="$(date -u +%Y%m%dT%H%M%SZ)"
OUT="$ROOT/.cache/p6/run-$STAMP"
JSON="$ROOT/results/bench/p6-emulator-$STAMP.json"
MODE="full"
for a in "$@"; do
    case "$a" in
        --smoke) MODE="smoke" ;;
        --self-test) exec python3 -I "$PY" --self-test ;;
        *) echo "run-p6.sh: unknown argument $a" >&2; exit 2 ;;
    esac
done

adbserial() {
    /usr/bin/adb devices 2>/dev/null | awk 'NR>1 && $1 ~ /^emulator-55[0-9][0-9]$/ {print $1}'
}
acos_qemu() { pgrep -f 'qemu-system-x86_64.*acos-oracle-34' 2>/dev/null || true; }

preflight() {
    local avail
    avail="$(free -m | awk '/^Mem:/ {print $7}')"
    echo "[p6] MemAvailable ${avail} MiB, loadavg1 $(cut -d' ' -f1 /proc/loadavg)"
    [ "${avail:-0}" -gt 16384 ] || { echo "[p6] too little RAM (${avail} MiB)" >&2; exit 1; }
    local q; q="$(acos_qemu)"
    [ -z "$q" ] || { echo "[p6] an acos-oracle emulator is already running: $q" >&2; exit 1; }
    local s; s="$(adbserial)"
    [ -z "$s" ] || { echo "[p6] adb already lists $s" >&2; exit 1; }
    echo "[p6] adb devices: $(/usr/bin/adb devices | sed '1d' | tr '\n' ';')"
    mkdir -p "$OUT" "$(dirname "$JSON")"
}

postflight() {
    # Belt and braces: guarded.sh kills its process group on timeout, and the
    # harness shuts the emulator down itself.  Verify, then force it.
    local q; q="$(acos_qemu)"
    if [ -n "$q" ]; then
        echo "[p6] LEFTOVER qemu after the run: $q — killing" >&2
        pkill -f 'qemu-system-x86_64.*acos-oracle-34' || true
        sleep 3
        pkill -9 -f 'qemu-system-x86_64.*acos-oracle-34' || true
        sleep 1
    fi
    q="$(acos_qemu)"
    if [ -n "$q" ]; then
        echo "[p6] FAILED to shut the emulator down: $q" >&2
        return 1
    fi
    echo "[p6] emulator shut down (no qemu-system-x86_64 for acos-oracle-34)"
    local s; s="$(adbserial)"
    [ -z "$s" ] && echo "[p6] adb lists no emulator-55xx" || echo "[p6] note: adb still lists $s"
    echo "[p6] MemAvailable now $(free -m | awk '/^Mem:/ {print $7}') MiB"
    echo "[p6] artifacts: $OUT ; $JSON"
}

preflight
set +e
ARGS=(--out "$OUT" --json "$JSON")
[ "$MODE" = smoke ] && ARGS+=(--quick)
"$ROOT/tools/art-runtime/run-exclusive.sh" --task P6 -- \
    "$ROOT/tools/guarded.sh" --mem 6G --time 2400 -- \
    python3 -I "$PY" "${ARGS[@]}" 2>&1 | tee "$OUT/driver.log"
rc=${PIPESTATUS[0]}
set -e
postflight || rc=1
echo "[p6] driver rc=$rc"
exit "$rc"
