#!/usr/bin/env bash
#
# run-framework-dexprog-art.sh — capture the ART reference for a dexprog that
# uses *framework* classes (`android.*`), e.g. android.database.sqlite.
#
# `run-dexprogs-art.sh` uses `dalvikvm64`, which is the bare ART CLI VM and does
# NOT run `AndroidRuntime::startReg`. Framework natives (SystemProperties,
# SQLiteConnection, CursorWindow, ...) are therefore unbound and every such
# program dies with UnsatisfiedLinkError. `app_process` starts AndroidRuntime,
# registers the framework natives, and can still run a plain `main`.
#
# Usage:
#   tools/oracle/run-framework-dexprog-art.sh 28_sqlite_basic
#
# The emulator must already be running (tools/oracle/emulator.sh start).
# Runs 3 times, refuses on non-determinism, and writes
# fixtures/expected-art/dexprogs/<name>.{stdout,stderr,exit}.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
SDK="${ANDROID_HOME:-${ANDROID_SDK_ROOT:-/home/Qui/Android/Sdk}}"
ADB="$SDK/platform-tools/adb"
SERIAL="${ACOS_ORACLE_SERIAL:-emulator-5554}"
DEVICE_DIR="/data/local/tmp/acos"
RUNS=3

log() { printf '[framework-dexprog-art] %s\n' "$*"; }
die() { printf '[framework-dexprog-art] error: %s\n' "$*" >&2; exit 1; }

[ $# -eq 1 ] || die "usage: $0 <dexprog-name>"
name="$1"
dex="$ROOT/fixtures/build/dexprogs/$name/classes.dex"
out_dir="$ROOT/fixtures/expected-art/dexprogs"
[ -x "$ADB" ] || die "missing $ADB"
[ -f "$dex" ] || die "missing $dex (run tools/build/build-fixtures.sh)"
"$ADB" -s "$SERIAL" get-state >/dev/null 2>&1 \
    || die "no device at $SERIAL (start it: tools/oracle/emulator.sh start)"

mkdir -p "$out_dir"
remote="$DEVICE_DIR/$name"
"$ADB" -s "$SERIAL" shell "rm -rf $remote && mkdir -p $remote" >/dev/null
"$ADB" -s "$SERIAL" push "$dex" "$remote/classes.dex" >/dev/null

work="$(mktemp -d "$ROOT/target/tmp/framework-dexprog.XXXXXX" 2>/dev/null \
    || mktemp -d)"
trap 'rm -rf "$work"' EXIT
mkdir -p "$ROOT/target/tmp"

for run in $(seq 1 "$RUNS"); do
    "$ADB" -s "$SERIAL" shell \
        "cd $remote && CLASSPATH=$remote/classes.dex app_process /system/bin Main >run$run.out 2>run$run.err; echo \$? >run$run.exit" \
        >/dev/null 2>&1 || true
    "$ADB" -s "$SERIAL" pull "$remote/run$run.out"  "$work/run$run.stdout" >/dev/null
    "$ADB" -s "$SERIAL" pull "$remote/run$run.err"  "$work/run$run.stderr" >/dev/null
    "$ADB" -s "$SERIAL" pull "$remote/run$run.exit" "$work/run$run.exit"   >/dev/null
done

for run in $(seq 2 "$RUNS"); do
    cmp -s "$work/run1.stdout" "$work/run$run.stdout" || die "$name non-deterministic stdout"
    cmp -s "$work/run1.stderr" "$work/run$run.stderr" || die "$name non-deterministic stderr"
    cmp -s "$work/run1.exit"   "$work/run$run.exit"   || die "$name non-deterministic exit"
done

cp "$work/run1.stdout" "$out_dir/$name.stdout"
cp "$work/run1.stderr" "$out_dir/$name.stderr"
cp "$work/run1.exit"   "$out_dir/$name.exit"
log "$name: deterministic over $RUNS runs; exit=$(tr -d '\r\n' <"$out_dir/$name.exit") stdout=$(wc -c <"$out_dir/$name.stdout") bytes"
log "wrote $out_dir/$name.{stdout,stderr,exit}"
