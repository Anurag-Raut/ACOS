#!/usr/bin/env bash
# build.sh -- build the E2D Java probe (javac + d8) and the JNI shim (NDK).
#
# Outputs into the E1A mini-root scratch so the bwrap root already sees them:
#   $ART_DATA/dex/e2d/classes.dex     -> /data/dex/e2d/classes.dex
#   $ART_DATA/lib/libe2dshim.so       -> /data/lib/libe2dshim.so
#   $ART_DATA/lib/libbindershim.so    -> /data/lib/libbindershim.so (LD_PRELOAD)
set -euo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
. "$HERE/../../a-dalvikvm/scripts/env.sh"

NDK="${ANDROID_NDK_HOME:-$HOME/Android/Sdk/ndk/27.1.12297006}"
CLANG="$NDK/toolchains/llvm/prebuilt/linux-x86_64/bin/x86_64-linux-android34-clang"
D8="$HOME/Android/Sdk/build-tools/35.0.0/d8"
OUT="${ART_DATA:-$ANDROID_DATA}"; mkdir -p "$OUT/dex/e2d" "$OUT/lib"
BUILD="$ACOS_ROOT/.cache/art-exp-d/build"; mkdir -p "$BUILD"

# One .so, three jobs: default-namespace dlopen of libandroid_runtime.so,
# fake /dev/binder, and the Java -> register_* JNI bridge.  It must be ONE
# object because bionic symbol lookup is namespace-scoped (see the file header).
echo "--- framework substrate shim ---"
"$CLANG" -O2 -fPIC -shared -Wall -Wno-unused-parameter \
    -o "$OUT/lib/libbindershim.so" "$HERE/../../d-framework/native/framework_shim.c" -llog -ldl

echo "--- java ---"
rm -rf "$BUILD/classes"; mkdir -p "$BUILD/classes"
SDKJAR="${ANDROID_JAR:-$HOME/Android/Sdk/platforms/android-34/android.jar}"
javac -nowarn -source 11 -target 11 -cp "$SDKJAR" \
      -d "$BUILD/classes" $(find "$HERE/../../d-framework/java" -name '*.java') 2>&1 |
    grep -v "bootstrap class path\|^1 warning\|warning: \[options\]" || true

echo "--- d8 ---"
rm -rf "$OUT/dex/e2d"; mkdir -p "$OUT/dex/e2d"
"$D8" --lib "$SDKJAR" --min-api 34 \
      --output "$OUT/dex/e2d" $(find "$BUILD/classes" -name '*.class') 2>&1 |
    grep -v "^Warning" || true
ls -la "$OUT/dex/e2d/classes.dex" "$OUT/lib/libbindershim.so"; rm -f "$OUT/lib/libe2dshim.so"
