#!/usr/bin/env bash
#
# emulator.sh — start/stop/status the dedicated ART oracle emulator.
#
#   tools/oracle/emulator.sh start    create AVD if needed, boot headless, wait
#                                     for sys.boot_completed, print boot time + RSS
#   tools/oracle/emulator.sh stop     shut the emulator down and verify it is gone
#   tools/oracle/emulator.sh status   report whether it is running
#
# Host limits (shared workstation, hard requirements):
#   * MemAvailable must be > 16 GiB before starting
#   * emulator RAM <= 2048 MB, cores <= 4, exactly ONE emulator instance
#   * -no-window -no-audio -no-boot-anim -no-snapshot-save -gpu swiftshader_indirect
#   * AVD lives under .cache/ (gitignored); ~/.android/avd is never touched.
#   * always stopped explicitly (the emulator is never left running).
#
# Everything is local: no sudo, no network downloads.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
SDK="${ANDROID_HOME:-${ANDROID_SDK_ROOT:-/home/Qui/Android/Sdk}}"
CACHE="${ACOS_CACHE:-$ROOT/.cache}"
AVD_NAME="${ACOS_ORACLE_AVD:-acos-oracle-34}"
AVD_HOME="$CACHE/avd"
EMU_HOME="$CACHE/emulator-home"
ORACLE_DIR="$CACHE/oracle"
LOG="$ORACLE_DIR/emulator.log"
PIDFILE="$ORACLE_DIR/emulator.pid"
PORT="${ACOS_ORACLE_PORT:-5554}"
SERIAL="emulator-$PORT"
SYSTEM_IMAGE="system-images;android-34;google_apis;x86_64"
RAM_MB=2048
CORES=4
BOOT_TIMEOUT="${ACOS_BOOT_TIMEOUT:-600}"
MIN_AVAIL_MB=16384

EMULATOR="$SDK/emulator/emulator"
ADB="$SDK/platform-tools/adb"
AVDMANAGER="$SDK/cmdline-tools/latest/bin/avdmanager"
[ -x "$EMULATOR" ]   || { echo "emulator.sh: missing $EMULATOR" >&2; exit 1; }
[ -x "$ADB" ]        || { echo "emulator.sh: missing $ADB" >&2; exit 1; }
[ -x "$AVDMANAGER" ] || { echo "emulator.sh: missing $AVDMANAGER" >&2; exit 1; }

export ANDROID_SDK_ROOT="$SDK"
export ANDROID_HOME="$SDK"
export ANDROID_AVD_HOME="$AVD_HOME"
export ANDROID_EMULATOR_HOME="$EMU_HOME"
mkdir -p "$AVD_HOME" "$EMU_HOME" "$ORACLE_DIR"

log() { printf '[oracle-emulator] %s\n' "$*"; }
die() { printf '[oracle-emulator] error: %s\n' "$*" >&2; exit 1; }

qemu_pids() { pgrep -f 'qemu-system-x86_64.*acos-oracle-[3]4' 2>/dev/null || true; }
emulator_serial_present() { "$ADB" devices 2>/dev/null | awk 'NR>1 {print $1}' | grep -qx "$SERIAL"; }

avd_exists() {
    [ -f "$AVD_HOME/$AVD_NAME.ini" ] && [ -d "$AVD_HOME/$AVD_NAME.avd" ]
}

avd_booted() {
    [ "$("$ADB" -s "$SERIAL" shell getprop sys.boot_completed 2>/dev/null | tr -d '\r')" = "1" ]
}

host_rss_kb() { # sum RSS of the emulator/qemu process tree
    local total=0 pid
    for pid in $(qemu_pids); do
        local rss
        rss="$(awk '/^VmRSS:/ {print $2}' "/proc/$pid/status" 2>/dev/null || echo 0)"
        total=$((total + ${rss:-0}))
    done
    printf '%s' "$total"
}

create_avd() {
    log "creating AVD $AVD_NAME under $AVD_HOME"
    # --device pixel_6 fixes the display profile; the system image is API 34.
    echo no | "$AVDMANAGER" create avd \
        -n "$AVD_NAME" \
        -k "$SYSTEM_IMAGE" \
        -d pixel_6 \
        --force >/dev/null 2>&1 \
        || die "avdmanager create avd failed (is $SYSTEM_IMAGE installed?)"
    local cfg="$AVD_HOME/$AVD_NAME.avd/config.ini"
    [ -f "$cfg" ] || die "AVD config not created: $cfg"
    # Enforce the host limits and headless-friendly settings in the AVD config.
    "$(command -v python3)" - "$cfg" "$RAM_MB" "$CORES" <<'PYEOF'
import re, sys
cfg, ram, cores = sys.argv[1], sys.argv[2], sys.argv[3]
overrides = {
    "hw.ramSize": ram,
    "hw.cpu.ncore": cores,
    "hw.gpu.enabled": "yes",
    "hw.gpu.mode": "swiftshader_indirect",
    "hw.keyboard": "yes",
    "hw.mainKeys": "no",
    "disk.dataPartition.size": "2G",
    "vm.heapSize": "256",
}
lines = open(cfg).read().splitlines()
seen = set()
out = []
for line in lines:
    key = line.split("=", 1)[0].strip()
    if key in overrides:
        out.append(f"{key}={overrides[key]}")
        seen.add(key)
    else:
        out.append(line)
for key, val in overrides.items():
    if key not in seen:
        out.append(f"{key}={val}")
open(cfg, "w").write("\n".join(out) + "\n")
PYEOF
}

start() {
    if [ -n "$(qemu_pids)" ]; then
        die "an emulator/qemu is already running (exactly one instance allowed): $(qemu_pids | tr '\n' ' ')"
    fi
    if emulator_serial_present; then
        die "$SERIAL is already listed by adb but no qemu process was found; clean it up first"
    fi

    local avail
    avail="$(free -m | awk '/^Mem:/ {print $7}')"
    log "MemAvailable: ${avail} MiB (require > ${MIN_AVAIL_MB} MiB)"
    [ "${avail:-0}" -gt "$MIN_AVAIL_MB" ] \
        || die "not enough free RAM: ${avail} MiB available, need > ${MIN_AVAIL_MB} MiB"

    avd_exists || create_avd

    # If anything in start fails after this point, never leave the emulator up.
    trap 'rc=$?; trap - EXIT; if [ "$rc" -ne 0 ]; then log "start failed (rc=$rc); stopping emulator"; stop >/dev/null 2>&1 || true; fi' EXIT

    local cfg="$AVD_HOME/$AVD_NAME.avd/config.ini"
    local ram cores
    ram="$(awk -F= '/^hw.ramSize[[:space:]]*=/ {print $2}' "$cfg" | tr -d ' ')"
    cores="$(awk -F= '/^hw.cpu.ncore[[:space:]]*=/ {print $2}' "$cfg" | tr -d ' ')"
    [ "${ram:-0}" -le 2048 ] || die "AVD RAM ${ram} MB exceeds the 2048 MB limit"
    [ "${cores:-0}" -le 4 ]   || die "AVD cores ${cores} exceed the 4-core limit"

    log "booting $AVD_NAME headless (ram=${ram}MB cores=${cores})"
    rm -f "$LOG" "$PIDFILE"
    "$ADB" start-server >/dev/null 2>&1 || true
    "$EMULATOR" -avd "$AVD_NAME" \
        -port "$PORT" \
        -no-window -no-audio -no-boot-anim -no-snapshot-save -no-snapshot-load \
        -gpu swiftshader_indirect \
        -memory "$RAM_MB" -cores "$CORES" \
        -no-metrics \
        >"$LOG" 2>&1 &
    local emu_pid=$!
    echo "$emu_pid" >"$PIDFILE"

    local t0 t_boot elapsed=0
    t0="$(date +%s)"
    log "waiting for sys.boot_completed (timeout ${BOOT_TIMEOUT}s)"
    while :; do
        if avd_booted; then
            t_boot="$(date +%s)"
            break
        fi
        if ! kill -0 "$emu_pid" 2>/dev/null && [ -z "$(qemu_pids)" ]; then
            tail -n 30 "$LOG" >&2 || true
            die "emulator process exited before boot completed"
        fi
        elapsed=$(( $(date +%s) - t0 ))
        if [ "$elapsed" -ge "$BOOT_TIMEOUT" ]; then
            tail -n 30 "$LOG" >&2 || true
            stop >/dev/null 2>&1 || true
            die "boot timed out after ${BOOT_TIMEOUT}s"
        fi
        sleep 2
    done

    local boot_wall=$(( t_boot - t0 ))
    local rss_kb
    rss_kb="$(host_rss_kb)"
    local rss_mib=$(( rss_kb / 1024 ))
    local avd_size
    avd_size="$(du -sh "$CACHE" 2>/dev/null | cut -f1)"
    log "BOOTED in ${boot_wall}s wall clock"
    log "emulator host RSS: ${rss_kb} KiB (${rss_mib} MiB); .cache size ${avd_size}"
    log "serial: $SERIAL"
    printf 'boot_wall_seconds=%s\nrss_kib=%s\nrss_mib=%s\nserial=%s\n' \
        "$boot_wall" "$rss_kb" "$rss_mib" "$SERIAL" >"$ORACLE_DIR/boot-info.txt"
}

stop() {
    local was_running=0
    if emulator_serial_present; then
        was_running=1
    fi
    if [ "$was_running" -eq 1 ]; then
        log "killing $SERIAL"
        "$ADB" -s "$SERIAL" emu kill >/dev/null 2>&1 || true
    fi
    # Fallback: kill our recorded PID / any qemu we started.
    if [ -f "$PIDFILE" ]; then
        local pid
        pid="$(cat "$PIDFILE" 2>/dev/null || true)"
        [ -n "$pid" ] && kill "$pid" 2>/dev/null || true
    fi
    local n=0
    while [ -n "$(qemu_pids)" ]; do
        n=$((n + 1))
        if [ "$n" -eq 15 ]; then
            log "qemu still alive after 30s, sending SIGKILL"
            pkill -f 'qemu-system-x86_64.*acos-oracle-[3]4' 2>/dev/null || true
        fi
        [ "$n" -gt 30 ] && die "could not stop the emulator"
        sleep 1
    done
    rm -f "$PIDFILE"
    log "emulator stopped; qemu processes: $(qemu_pids | wc -l)"
}

status() {
    local pids
    pids="$(qemu_pids)"
    if [ -z "$pids" ]; then
        log "not running (no qemu-system process)"
        return 1
    fi
    log "running: qemu pids $(echo "$pids" | tr '\n' ' ')"
    log "host RSS: $(( $(host_rss_kb) / 1024 )) MiB"
    log "adb serial: $SERIAL present=$(emulator_serial_present && echo yes || echo no)"
    log "sys.boot_completed=$( "$ADB" -s "$SERIAL" shell getprop sys.boot_completed 2>/dev/null | tr -d '\r' || true)"
    return 0
}

case "${1:-}" in
    start)  start ;;
    stop)   stop ;;
    status) status ;;
    *) echo "usage: $0 {start|stop|status}" >&2; exit 2 ;;
esac
