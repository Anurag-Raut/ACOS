#!/usr/bin/env bash
# build.sh -- build the E3G artefacts (NDK clang for the interposer, rustc for
# the peer, javac + d8 for the test dex), into the E1A mini-root and .cache/art-exp-g.
set -euo pipefail
E3G_HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
. "$E3G_HERE/../../a-dalvikvm/scripts/env.sh"

NDK="${ANDROID_NDK_HOME:-$HOME/Android/Sdk/ndk/27.1.12297006}"
CLANG="$NDK/toolchains/llvm/prebuilt/linux-x86_64/bin/x86_64-linux-android34-clang"
D8="$HOME/Android/Sdk/build-tools/35.0.0/d8"
SCRATCH="$ACOS_ROOT/.cache/art-exp-g"
OUT="${ART_DATA:-$ANDROID_DATA}"
mkdir -p "$SCRATCH/build" "$OUT/dex/e3g" "$OUT/lib"

echo "--- interposer (bionic, x86_64, API 34) ---"
"$CLANG" -O2 -fPIC -shared -Wall -Wno-unused-parameter -Wno-unused-function \
    -o "$OUT/lib/libbindere3g.so" "$E3G_HERE/../native/binder_driver.c" -llog -ldl
cp -f "$OUT/lib/libbindere3g.so" "$SCRATCH/build/libbindere3g.so"

echo "--- peer (rustc, no crates) ---"
rustc -O --edition 2021 -A warnings -o "$SCRATCH/build/e3g-peer" "$E3G_HERE/../peer/src/main.rs"

echo "--- java ---"
rm -rf "$SCRATCH/classes"; mkdir -p "$SCRATCH/classes"
SDKJAR="${ANDROID_JAR:-$HOME/Android/Sdk/platforms/android-34/android.jar}"
javac -nowarn -source 11 -target 11 -cp "$SDKJAR" \
      -d "$SCRATCH/classes" $(find "$E3G_HERE/../java" -name '*.java') 2>&1 |
    grep -v "bootstrap class path\|^1 warning\|warning: \[options\]" || true

echo "--- d8 ---"
rm -rf "$OUT/dex/e3g"; mkdir -p "$OUT/dex/e3g"
"$D8" --lib "$SDKJAR" --min-api 34 \
      --output "$OUT/dex/e3g" $(find "$SCRATCH/classes" -name '*.class') 2>&1 |
    grep -v "^Warning" || true
ls -la "$OUT/dex/e3g/classes.dex" "$OUT/lib/libbindere3g.so" "$SCRATCH/build/e3g-peer"
