#!/usr/bin/env bash
#
# Build the W84-A React Native / Hermes probe assets:
#
#   fixtures/build/rn-hermes/x86_64/libhermesprobe.so
#       our own NDK-built fixture library; it initialises a Hermes runtime and
#       evaluates JS against the *same* `libhermes.so`/`libjsi.so` the app uses.
#
#   crates/acos-jni/tests/data/rn-hermes-probe.dex
#       the Java harness entry (`HermesProbe`) that drives `System.loadLibrary`,
#       the merged `OpenSourceMergedSoMapping` natives and the probe.
#
# Everything comes from the already-populated caches under `.cache/` (the
# RN 0.81.4 AARs and `.cache/rn-w76a/lib`); nothing is downloaded.
#
# Usage: tools/probes/rn-hermes-probe.sh
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
SDK="${ANDROID_HOME:-/home/Qui/Android/Sdk}"
NDK="${ANDROID_NDK_HOME:-$(ls -d "$SDK"/ndk/* 2>/dev/null | sort -V | tail -1)}"
BT="${BUILD_TOOLS_VERSION:-35.0.0}"
D8="$SDK/build-tools/$BT/d8"
CACHE_RN="$ROOT/.cache/rn-0.81.4"
LIBDIR="$ROOT/.cache/rn-w76a/lib"
OUT_DIR="$ROOT/fixtures/build/rn-hermes"
WORK="$ROOT/target/tmp/rn-hermes-probe"

die() { echo "rn-hermes-probe: error: $*" >&2; exit 1; }
log() { echo "rn-hermes-probe: $*"; }

[ -n "$NDK" ] || die "no Android NDK found under $SDK/ndk"
CXX="$(ls "$NDK"/toolchains/llvm/prebuilt/linux-x86_64/bin/x86_64-linux-android*-clang++ 2>/dev/null | sort -V | tail -1)"
[ -x "$CXX" ] || die "x86_64 clang++ not found under $NDK"
[ -x "$D8" ] || die "d8 not found: $D8"

for lib in libhermes.so libjsi.so libc++_shared.so; do
    [ -f "$LIBDIR/$lib" ] || die "missing $LIBDIR/$lib (run the rn_native_stack test first)"
done
[ -f "$CACHE_RN/aar/hermes-android-0.81.4-release.aar" ] || die "hermes-android AAR missing (run tools/build/build-rn-hello.sh fetch)"
[ -f "$CACHE_RN/aar/react-android-0.81.4-release.aar" ] || die "react-android AAR missing (run tools/build/build-rn-hello.sh fetch)"

rm -rf "$WORK"
mkdir -p "$WORK/headers" "$WORK/classes" "$WORK/dex" "$OUT_DIR/x86_64"

# --- 1. headers (Hermes prefab + JSI source) -------------------------------
log "extracting Hermes prefab headers"
unzip -o -q "$CACHE_RN/aar/hermes-android-0.81.4-release.aar" \
    'prefab/modules/libhermes/include/*' -d "$WORK/headers"
HERMES_INC="$WORK/headers/prefab/modules/libhermes/include"
JSI_INC="$CACHE_RN/rnjs/package/ReactCommon/jsi"
[ -f "$HERMES_INC/hermes/hermes.h" ] || die "hermes.h not in the prefab headers"
[ -f "$JSI_INC/jsi/jsi.h" ] || die "jsi/jsi.h not found at $JSI_INC"

# --- 2. our probe .so ------------------------------------------------------
log "compiling libhermesprobe.so ($CXX)"
"$CXX" --target=x86_64-linux-android26 -std=c++17 -fPIC -shared -O1 \
    -I"$HERMES_INC" -I"$JSI_INC" \
    -o "$OUT_DIR/x86_64/libhermesprobe.so" \
    "$ROOT/fixtures/apps/rn-hello/native/hermes_probe.cpp" \
    -L"$LIBDIR" -lhermes -ljsi -lc++_shared
log "  -> $OUT_DIR/x86_64/libhermesprobe.so ($(stat -c '%s' "$OUT_DIR/x86_64/libhermesprobe.so") bytes)"

# --- 3. the Java driver dex ------------------------------------------------
log "extracting compile classpath"
mkdir -p "$WORK/cp"
for aar in react-android hermes-android; do
    unzip -o -q "$CACHE_RN/aar/${aar}-0.81.4-release.aar" classes.jar -d "$WORK/cp/$aar"
done
unzip -o -q "$HOME/.gradle/caches/modules-2/files-2.1/com.facebook.soloader/soloader/0.12.1"/*/soloader-0.12.1.aar \
    classes.jar -d "$WORK/cp/soloader"
CP="$WORK/cp/react-android/classes.jar:$WORK/cp/hermes-android/classes.jar:$WORK/cp/soloader/classes.jar"

log "javac HermesProbe"
javac --release 11 -encoding UTF-8 -cp "$CP" -d "$WORK/classes" \
    "$ROOT/crates/acos-jni/tests/java/dev/acos/fixtures/rnhello/HermesProbe.java"

log "d8 HermesProbe"
"$D8" --min-api 26 --output "$WORK/dex" \
    "$WORK/classes/dev/acos/fixtures/rnhello/HermesProbe.class"
cp "$WORK/dex/classes.dex" "$ROOT/crates/acos-jni/tests/data/rn-hermes-probe.dex"
log "  -> crates/acos-jni/tests/data/rn-hermes-probe.dex"
log "done"
