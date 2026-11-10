#!/usr/bin/env bash
#
# run-dexprogs-art.sh — run every pure-Java fixture DEX on the REAL Android 14
# ART (API 34 emulator) and capture the authoritative `art-reference` outputs.
#
# For each fixtures/build/dexprogs/<name>/classes.dex:
#   * adb push to /data/local/tmp/acos/<name>/
#   * run `dalvikvm64 -cp classes.dex Main` (dalvikvm64 is the ART CLI VM
#     shipped in /apex/com.android.art/bin on API 34; app_process is not needed
#     for a plain console main)
#   * redirect stdout/stderr to device files and the exit code to a third file,
#     then `adb pull` the three files.  Redirecting on the device (rather than
#     parsing `adb shell` output) preserves raw bytes and keeps stdout/stderr
#     strictly separate — no adb CRLF translation or stream merging.
#
# Each program is run 3 times; any non-determinism in stdout/stderr/exit is
# recorded.  Outputs go to fixtures/expected-art/dexprogs/<name>.{stdout,stderr,exit}.
#
# The emulator must already be running (tools/oracle/emulator.sh start).
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
SDK="${ANDROID_HOME:-${ANDROID_SDK_ROOT:-/home/Qui/Android/Sdk}}"
ADB="$SDK/platform-tools/adb"
SERIAL="${ACOS_ORACLE_SERIAL:-emulator-5554}"
BUILD_DIR="$ROOT/fixtures/build/dexprogs"
OUT_DIR="$ROOT/fixtures/expected-art/dexprogs"
RESULT_DIR="$ROOT/results/oracle"
DEVICE_DIR="/data/local/tmp/acos"
RUNS=3

log() { printf '[run-dexprogs-art] %s\n' "$*"; }
die() { printf '[run-dexprogs-art] error: %s\n' "$*" >&2; exit 1; }

[ -x "$ADB" ] || die "missing $ADB"
"$ADB" -s "$SERIAL" get-state >/dev/null 2>&1 || die "no device at $SERIAL (start it: tools/oracle/emulator.sh start)"

if [ -z "$(find "$BUILD_DIR" -mindepth 2 -maxdepth 2 -name 'classes.dex' -print -quit 2>/dev/null)" ]; then
    log "fixtures/build/dexprogs is empty; running tools/build-fixtures.sh"
    bash "$ROOT/tools/build-fixtures.sh"
fi
[ -d "$BUILD_DIR" ] || die "no built fixtures at $BUILD_DIR"

mkdir -p "$OUT_DIR" "$RESULT_DIR"
log "serial=$SERIAL  device dir=$DEVICE_DIR  runs=$RUNS"
"$ADB" -s "$SERIAL" shell "rm -rf $DEVICE_DIR && mkdir -p $DEVICE_DIR" >/dev/null

nd_report="$RESULT_DIR/dexprogs-nondeterminism.txt"
runs_json="$RESULT_DIR/dexprogs-runs.json"
: >"$nd_report"
: >"$runs_json"
printf '{\n' >>"$runs_json"

programs=("$BUILD_DIR"/*/)
count=0
first_json=1
for dir in "${programs[@]}"; do
    name="$(basename "$dir")"
    dex="$dir/classes.dex"
    [ -f "$dex" ] || continue

    remote="$DEVICE_DIR/$name"
    "$ADB" -s "$SERIAL" shell "mkdir -p $remote" >/dev/null
    "$ADB" -s "$SERIAL" push "$dex" "$remote/classes.dex" >/dev/null \
        || die "adb push failed for $name"

    rm -rf "$dir/art-runs"
    mkdir -p "$dir/art-runs"
    for run in $(seq 1 "$RUNS"); do
        # Redirect on the device: stdout, stderr and $? travel as exact bytes.
        "$ADB" -s "$SERIAL" shell \
            "cd $remote && dalvikvm64 -cp classes.dex Main >run$run.out 2>run$run.err; echo \$? >run$run.exit" \
            >/dev/null 2>&1 || true
        "$ADB" -s "$SERIAL" pull "$remote/run$run.out"  "$dir/art-runs/run$run.stdout" >/dev/null
        "$ADB" -s "$SERIAL" pull "$remote/run$run.err"  "$dir/art-runs/run$run.stderr" >/dev/null
        "$ADB" -s "$SERIAL" pull "$remote/run$run.exit" "$dir/art-runs/run$run.exit" >/dev/null
    done

    cp "$dir/art-runs/run1.stdout" "$OUT_DIR/$name.stdout"
    cp "$dir/art-runs/run1.stderr" "$OUT_DIR/$name.stderr"
    cp "$dir/art-runs/run1.exit"   "$OUT_DIR/$name.exit"

    # determinism check across all runs
    determ=yes
    for run in $(seq 2 "$RUNS"); do
        cmp -s "$dir/art-runs/run1.stdout" "$dir/art-runs/run$run.stdout" || determ=no
        cmp -s "$dir/art-runs/run1.stderr" "$dir/art-runs/run$run.stderr" || determ=no
        cmp -s "$dir/art-runs/run1.exit"   "$dir/art-runs/run$run.exit"   || determ=no
    done
    exit_code="$(tr -d '\r\n' <"$OUT_DIR/$name.exit")"
    if [ "$determ" = no ]; then
        printf '%s\n' "$name" >>"$nd_report"
        log "  !! NON-DETERMINISTIC: $name"
    fi

    printf '  %-28s exit=%-3s stdout=%-5s stderr=%-5s %s\n' \
        "$name" "$exit_code" \
        "$(stat -c '%s' "$OUT_DIR/$name.stdout")" \
        "$(stat -c '%s' "$OUT_DIR/$name.stderr")" \
        "$([ "$determ" = no ] && echo NONDETERMINISTIC || echo deterministic)"

    [ "$first_json" -eq 1 ] || printf ',\n' >>"$runs_json"
    first_json=0
    printf '  "%s": {"exit": %s, "nondeterministic": %s, "runs": %s}' \
        "$name" "${exit_code:-0}" "$([ "$determ" = no ] && echo true || echo false)" "$RUNS" >>"$runs_json"
    count=$((count + 1))
done
printf '\n}\n' >>"$runs_json"

if [ -s "$nd_report" ]; then
    log "nondeterministic programs: $(paste -sd, "$nd_report")"
else
    log "all $count programs deterministic across $RUNS runs"
fi
log "wrote $count programs -> $OUT_DIR"
