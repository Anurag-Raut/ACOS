#!/usr/bin/env bash
#
# parity live -- compare the attached phone's UI with ACOS's live View tree.
#
# Installs ONE of our fixture APKs on the explicitly named device, launches it,
# captures `uiautomator dump` + a screenshot, captures the ACOS tree from the
# real View hierarchy, compares them and emits JSON. It only touches packages
# under `dev.acos.fixtures.` and never changes device settings.
#
# The live path is optional. If adb is missing or the device is not attached,
# this script exits 3 with a loud message: it never reports an empty pass.
#
# Usage:
#   tools/parity/live.sh [APK] [--keep]
#   ACOS_DEVICE=ZA222WGJPP tools/parity/live.sh fixtures/build/apps/counter.apk
set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
BIN="${ACOS_BIN:-$ROOT/target/release/acos}"
SERIAL="${ACOS_DEVICE:-ZA222WGJPP}"
APK="${1:-$ROOT/fixtures/build/apps/counter.apk}"
OUT="${ACOS_PARITY_OUT:-$ROOT/results/parity/live-$(date +%Y%m%d-%H%M%S)}"
KEEP=()
if [ "${2:-}" = "--keep" ]; then KEEP=(--keep); fi

if ! command -v adb >/dev/null 2>&1; then
    echo "parity-live: adb is not installed; SKIPPED (no false pass)" >&2
    exit 3
fi
if ! adb -s "$SERIAL" get-state >/dev/null 2>&1; then
    echo "parity-live: device $SERIAL is not attached; SKIPPED (no false pass)" >&2
    exit 3
fi
if [ ! -x "$BIN" ]; then
    echo "parity-live: $BIN is not executable; build it first (cargo build -p acos-cli)" >&2
    exit 2
fi
if [ ! -f "$APK" ]; then
    echo "parity-live: APK $APK not found; run tools/build-apps.sh first" >&2
    exit 2
fi

mkdir -p "$OUT"
# The parity command enforces the dev.acos.fixtures.* package rule itself.
set +e
tools/guarded.sh --mem 8G --time 600 -- "$BIN" parity \
    --device "$SERIAL" "$APK" --output "$OUT" "${KEEP[@]}" --json | tee "$OUT/parity.json"
status=${PIPESTATUS[0]}
set -e
echo "parity-live: artifacts in $OUT" >&2
exit "$status"
