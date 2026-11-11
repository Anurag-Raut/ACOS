#!/usr/bin/env bash
# build.sh -- build the E4H probe (javac + d8) into the E1A mini-root.
# Reuses E2D's libbindershim.so (Nat.register) untouched: the dex goes to
# /data/dex/q3/classes.dex and is run with /data/dex/e2d/classes.dex on the
# classpath so dev.acos.e2d.Nat is available.
set -eo pipefail
E4H="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
. "$E4H/../../a-dalvikvm/scripts/env.sh"

D8="$(ls -d "$HOME"/Android/Sdk/build-tools/*/d8 2>/dev/null | tail -1)"
SDKJAR="${ANDROID_JAR:-$HOME/Android/Sdk/platforms/android-34/android.jar}"
OUT="${ART_DATA:-$ANDROID_DATA}"; mkdir -p "$OUT/dex/q3" "$OUT/out"
BUILD="$ACOS_ROOT/.cache/art-exp-q3/build"; mkdir -p "$BUILD"

rm -rf "$BUILD/classes"; mkdir -p "$BUILD/classes"
# dev.acos.e2d.Nat is compiled from source (the JNI symbol names are bound to
# that class); its .class is dropped again so it is not duplicated in our dex.
javac -nowarn -source 11 -target 11 -cp "$SDKJAR" -d "$BUILD/classes" \
      -sourcepath "$E4H/../java:$E4H/../../d-framework/java" \
      $(find "$E4H/../java" -name '*.java') 2>&1 |
    grep -v "bootstrap class path\|^1 warning\|warning: \[options\]" || true
rm -rf "$BUILD/classes/dev/acos/e2d"

rm -rf "$OUT/dex/q3"; mkdir -p "$OUT/dex/q3"
"$D8" --lib "$SDKJAR" --min-api 34 --output "$OUT/dex/q3" \
      $(find "$BUILD/classes" -name '*.class') 2>&1 | grep -v "^Warning" || true
echo "--- crash tracer (bionic, LD_PRELOAD) ---"
NDK="${ANDROID_NDK_HOME:-$HOME/Android/Sdk/ndk/27.1.12297006}"
CLANG="$NDK/toolchains/llvm/prebuilt/linux-x86_64/bin/x86_64-linux-android34-clang"
[ -x "$CLANG" ] || { echo "no NDK clang at $CLANG" >&2; exit 1; }
mkdir -p "$OUT/lib"
"$CLANG" -O2 -fPIC -shared -Wall -Wno-unused-parameter -o "$OUT/lib/libcrashtrace.so" \
    "$E4H/../native/crashtrace.c" -ldl
ls -la "$OUT/dex/q3/classes.dex" "$OUT/lib/libcrashtrace.so"
cp -f "$ACOS_ROOT/fixtures/build/apps/counter.apk" "$OUT/apk/counter.apk"
ls -la "$OUT/apk/counter.apk"
