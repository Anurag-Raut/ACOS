#!/usr/bin/env bash
#
# capture-device-reference.sh — capture `physical-device` evidence for the ACOS
# fixture apps on a connected physical Android phone.
#
# This is the physical-device sibling of capture-app-reference.sh. It records,
# under fixtures/reference-device/<app>/:
#   * DEVICE.md / device.json   device facts (serial stored only as a hash)
#   * screenshot-NN-*.png       agent-device screenshot per scenario step
#   * snapshot-NN-*.json/.txt   agent-device UI snapshot tree per step
#   * logcat-NN-*.txt           logcat excerpt filtered to the app process
#   * manifest.json             tier, device facts, step names, timestamps
# plus fixtures/reference-device/COMPARISON.md comparing the device's final
# state against the emulator ART reference and the scenario's expectations.
#
# Fidelity label: `physical-device`. It is never mixed silently with
# `art-reference (emulator, API 34, google_apis x86_64)`.
#
# The device is driven through agent-device (taps/fills by id/label selector),
# never raw `input tap`. Raw adb is used only for device facts, app-state clear,
# logcat and uninstall fallback.
#
# Safety: only the four ACOS fixture packages are installed/uninstalled; no
# account/security/connectivity/lock setting is touched; rotation goes through
# `agent-device orientation` and is restored to `auto` at the end.
#
# Usage:
#   tools/oracle/capture-device-reference.sh [app ...]
#   ACOS_DEVICE_SERIAL=ZA222WGJPP tools/oracle/capture-device-reference.sh
#
# The APKs must already exist (tools/build/build-apps.sh). The device must be
# connected, unlocked and authorised for USB debugging.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
SDK="${ANDROID_HOME:-${ANDROID_SDK_ROOT:-/home/Qui/Android/Sdk}}"
ADB="$SDK/platform-tools/adb"
SERIAL="${ACOS_DEVICE_SERIAL:-ZA222WGJPP}"
OUT_ROOT="${ACOS_DEVICE_REFERENCE_DIR:-$ROOT/fixtures/reference-device}"
APPS_DIR="${ACOS_APPS_DIR:-$ROOT/fixtures/build/apps}"
SCENARIO_DIR="${ACOS_SCENARIO_DIR:-$ROOT/scenarios}"
EMULATOR_REF="${ACOS_EMULATOR_REF:-$ROOT/fixtures/reference}"
AGENT_DEVICE="${AGENT_DEVICE_BIN:-agent-device}"
PYTHON="${PYTHON:-python3}"

DEFAULT_APPS=(counter todo-list multi-activity form-validation)
if [ "$#" -gt 0 ]; then
    APPS=("$@")
else
    APPS=("${DEFAULT_APPS[@]}")
fi

log() { printf '[capture-device-ref] %s\n' "$*"; }
die() { printf '[capture-device-ref] error: %s\n' "$*" >&2; exit 1; }

command -v "$PYTHON" >/dev/null 2>&1 || die "python3 not found"
command -v "$AGENT_DEVICE" >/dev/null 2>&1 || die "agent-device not found (AGENT_DEVICE_BIN)"
[ -x "$ADB" ] || die "adb not found at $ADB"

"$ADB" -s "$SERIAL" get-state >/dev/null 2>&1 \
    || die "no device at serial $SERIAL (connect the phone and authorise USB debugging)"

log "device: $SERIAL ($("$ADB" -s "$SERIAL" shell getprop ro.product.model | tr -d '\r'))"
log "output: $OUT_ROOT"
log "apps: ${APPS[*]}"

"$PYTHON" "$ROOT/tools/oracle/capture-device-reference.py" \
    --root "$ROOT" \
    --serial "$SERIAL" \
    --adb "$ADB" \
    --agent-device "$AGENT_DEVICE" \
    --out-root "$OUT_ROOT" \
    --apps-dir "$APPS_DIR" \
    --scenario-dir "$SCENARIO_DIR" \
    --emulator-ref "$EMULATOR_REF" \
    --apps "${APPS[@]}"
