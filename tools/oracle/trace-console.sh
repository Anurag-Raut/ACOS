#!/usr/bin/env bash
#
# trace-console.sh — measure what the real ART core library actually executes
# for the pure-Java console fixtures, on API 34 (x86_64, KVM emulator).
#
# For each of 01_hello, 17_generics_collections, 19_threads_sync it runs:
#   dalvikvm64 -Xmethod-trace -Xmethod-trace-file:... -Xmethod-trace-file-size:...
#               -verbose:jni -cp classes.dex Main
# and captures:
#   * the ART method trace (raw, under .cache/trace/console/)
#   * -verbose:jni to logcat (JNI native-method registrations)
#   * the program stdout/exit
# then parses the trace with art_trace.py into research/dynamic-trace/data/:
#   methods_<prog>.tsv, natives_<prog>.tsv, classes_initialised_<prog>.txt,
#   trace_<prog>.json
#
# It also records, for the boot-image question, a comparison of default vs
# -Ximage:/-Xnoimage-dex2oat variants (data/console_image_variants.txt).
#
# The native-method set used to classify traced methods is generated from the
# real boot-classpath DEX with the SDK dexdump (data/native_methods_bootclasspath.tsv).
#
# Requirements: emulator running (tools/oracle/emulator.sh start), adb root not
# required but used when available.
set -euo pipefail
export PYTHONDONTWRITEBYTECODE=1

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
SDK="${ANDROID_HOME:-${ANDROID_SDK_ROOT:-/home/Qui/Android/Sdk}}"
ADB="$SDK/platform-tools/adb"
DEXDUMP="$SDK/build-tools/${BUILD_TOOLS_VERSION:-35.0.0}/dexdump"
SERIAL="${ACOS_ORACLE_SERIAL:-emulator-5554}"
BOOT="$ROOT/.cache/boot/android-34"
DATA_DIR="$ROOT/research/dynamic-trace/data"
RAW_DIR="$ROOT/.cache/trace/console"
DEVICE_DIR="/data/local/tmp/acos-trace"
PROGS=(01_hello 17_generics_collections 19_threads_sync)
TRACE_SIZE=134217728

log() { printf '[trace-console] %s\n' "$*"; }
die() { printf '[trace-console] error: %s\n' "$*" >&2; exit 1; }

"$ADB" -s "$SERIAL" get-state >/dev/null 2>&1 || die "no device at $SERIAL"
[ -f "$BOOT/BOOTCLASSPATH" ] || die "boot classpath not extracted; run tools/fetch/extract-bootclasspath.sh"
mkdir -p "$DATA_DIR" "$RAW_DIR"

# --- 1. native-method set from the real boot-classpath DEX -------------------
NATIVE_SET="$DATA_DIR/native_methods_bootclasspath.tsv"
if [ ! -s "$NATIVE_SET" ] || [ "${ACOS_REGEN_NATIVE_SET:-0}" = 1 ]; then
    log "generating native-method set from boot-classpath DEX (dexdump)"
    : >"$NATIVE_SET"
    while IFS= read -r j; do
        n="${j%.jar}"
        for d in "$BOOT/dex/$n"/classes*.dex; do
            [ -f "$d" ] || continue
            "$DEXDUMP" "$d" 2>/dev/null | python3 "$ROOT/tools/oracle/dex_native_methods.py" >>"$NATIVE_SET"
        done
    done <"$BOOT/BOOTCLASSPATH"
    sort -u -o "$NATIVE_SET" "$NATIVE_SET"
fi
log "native-method set: $(wc -l <"$NATIVE_SET") methods"

# --- 2. build fixtures if needed --------------------------------------------
if [ -z "$(find "$ROOT/fixtures/build/dexprogs" -mindepth 2 -maxdepth 2 -name classes.dex -print -quit 2>/dev/null)" ]; then
    log "building fixtures"
    bash "$ROOT/tools/build/build-fixtures.sh"
fi

"$ADB" -s "$SERIAL" shell "rm -rf $DEVICE_DIR && mkdir -p $DEVICE_DIR" >/dev/null

label_for() { # 01_hello -> println ; others keep their name
    case "$1" in
        01_hello) echo "println" ;;
        *) echo "$1" ;;
    esac
}

for prog in "${PROGS[@]}"; do
    dex="$ROOT/fixtures/build/dexprogs/$prog/classes.dex"
    [ -f "$dex" ] || die "missing $dex"
    label="$(label_for "$prog")"
    remote="$DEVICE_DIR/$prog"
    log "=== $prog (label=$label) ==="
    "$ADB" -s "$SERIAL" shell "mkdir -p $remote" >/dev/null
    "$ADB" -s "$SERIAL" push "$dex" "$remote/classes.dex" >/dev/null
    "$ADB" -s "$SERIAL" shell "logcat -c" >/dev/null 2>&1 || true
    "$ADB" -s "$SERIAL" shell \
        "cd $remote && rm -f $prog.trace && dalvikvm64 \
            -Xmethod-trace \
            -Xmethod-trace-file:$remote/$prog.trace \
            -Xmethod-trace-file-size:$TRACE_SIZE \
            -verbose:jni \
            -cp classes.dex Main \
            >$prog.stdout 2>$prog.stderr; echo \$? >$prog.exit" >/dev/null 2>&1 || true
    "$ADB" -s "$SERIAL" pull "$remote/$prog.trace" "$RAW_DIR/$prog.trace" >/dev/null
    "$ADB" -s "$SERIAL" pull "$remote/$prog.stdout" "$RAW_DIR/$prog.stdout" >/dev/null
    "$ADB" -s "$SERIAL" pull "$remote/$prog.stderr" "$RAW_DIR/$prog.stderr" >/dev/null
    "$ADB" -s "$SERIAL" pull "$remote/$prog.exit" "$RAW_DIR/$prog.exit" >/dev/null
    "$ADB" -s "$SERIAL" shell "logcat -d -s dalvikvm64" >"$RAW_DIR/$prog.logcat.txt" 2>&1 || true
    python3 "$ROOT/tools/oracle/art_trace.py" parse "$RAW_DIR/$prog.trace" \
        --native-set "$NATIVE_SET" \
        --main-class Main \
        --label "$label" \
        --json "$DATA_DIR/trace_$label.json" \
        --data-dir "$DATA_DIR" \
        --top 10
    # JNI registration lines (native method binding)
    grep -o 'Registering JNI native method .*' "$RAW_DIR/$prog.logcat.txt" \
        | sed 's/^Registering JNI native method //' >"$DATA_DIR/jni_registrations_$label.txt" || true
    log "  registrations: $(wc -l <"$DATA_DIR/jni_registrations_$label.txt")  exit=$(tr -d '\r\n' <"$RAW_DIR/$prog.exit")"
done

# --- 3. boot-image variant comparison (01_hello) ----------------------------
VARIANT_OUT="$DATA_DIR/console_image_variants.txt"
{
    echo "01_hello boot-image / init variants (each run with -Xmethod-trace)."
    echo "columns: variant methods clinit natives bytes_exit"
} >"$VARIANT_OUT"
for opt in "DEFAULT" "-Ximage:" "-Xnoimage-dex2oat" "-Xint"; do
    tag="$(echo "$opt" | tr -c 'A-Za-z0-9' '_')"
    remote="$DEVICE_DIR/01_hello"
    if [ "$opt" = "DEFAULT" ]; then opts=""; else opts="$opt"; fi
    "$ADB" -s "$SERIAL" shell \
        "cd $remote && rm -f var.trace && dalvikvm64 $opts \
            -Xmethod-trace -Xmethod-trace-file:$remote/var.trace \
            -Xmethod-trace-file-size:$TRACE_SIZE -cp classes.dex Main >/dev/null 2>&1; echo \$?" >/dev/null 2>&1 || true
    "$ADB" -s "$SERIAL" pull "$remote/var.trace" "$RAW_DIR/01_hello.$tag.trace" >/dev/null 2>&1 || true
    tmpjson="$(mktemp)"
    python3 "$ROOT/tools/oracle/art_trace.py" parse "$RAW_DIR/01_hello.$tag.trace" \
        --native-set "$NATIVE_SET" --main-class Main --label "$tag" \
        --json "$tmpjson" >/dev/null 2>&1 || true
    python3 - "$opt" "$tmpjson" >>"$VARIANT_OUT" <<'PYEOF'
import json, sys
opt, path = sys.argv[1], sys.argv[2]
try:
    d = json.load(open(path))
except Exception as exc:
    print(opt, "ERROR", exc)
    sys.exit()
print("%s methods=%d clinit=%d natives=%d" % (
    opt, d["distinct_methods_executed"], len(d["classes_initialised"]), len(d["natives"])))
PYEOF
    rm -f "$tmpjson"
done

# --- 4. -verbose:class observability check --------------------------------
"$ADB" -s "$SERIAL" shell "logcat -c" >/dev/null 2>&1 || true
"$ADB" -s "$SERIAL" shell \
    "cd $DEVICE_DIR/01_hello && dalvikvm64 -verbose:class -cp classes.dex Main >/dev/null 2>&1" >/dev/null 2>&1 || true
"$ADB" -s "$SERIAL" shell "logcat -d -b all" >"$RAW_DIR/01_hello.verbose_class.logcat.txt" 2>&1 || true
{
    echo "-verbose:class observability on this image (userdebug, API 34):"
    echo "  logcat lines matching '\[Loaded': $(grep -c '\[Loaded' "$RAW_DIR/01_hello.verbose_class.logcat.txt" || true)"
    echo "  logcat lines matching 'Initialized': $(grep -c 'Initialized' "$RAW_DIR/01_hello.verbose_class.logcat.txt" || true)"
    echo "  => class load/init order is compiled out of this libart (VLOG); derive"
    echo "     initialisation from <clinit> records in the method trace instead."
} >"$DATA_DIR/verbose_class_note.txt"

log "done. Parsed data in $DATA_DIR; raw traces in $RAW_DIR"
