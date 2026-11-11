#!/usr/bin/env bash
# Build the P2d artefacts: the bionic userspace-binder driver, the host peer
# (real ServiceHost), and the Java acceptance dex.
set -euo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
CRATE="$(cd "$HERE/.." && pwd)"
. "$CRATE/../../research/art-feasibility/a-dalvikvm/scripts/env.sh"

NDK="${ANDROID_NDK_HOME:-$HOME/Android/Sdk/ndk/27.1.12297006}"
CLANG="$NDK/toolchains/llvm/prebuilt/linux-x86_64/bin/x86_64-linux-android34-clang"
D8="$HOME/Android/Sdk/build-tools/35.0.0/d8"
SDKJAR="${ANDROID_JAR:-$HOME/Android/Sdk/platforms/android-34/android.jar}"
SCRATCH="$ACOS_ROOT/.cache/art-exp-p2d"
OUT="${ART_DATA:-$ANDROID_DATA}"
mkdir -p "$SCRATCH/build" "$OUT/dex/binderpeer" "$OUT/lib"

echo "--- driver (bionic, x86_64, API 34) ---"
"$CLANG" -O2 -fPIC -shared -Wall -Wno-unused-parameter -Wno-unused-function \
    -o "$OUT/lib/libacos_binder_driver.so" "$CRATE/native/binder_driver.c" -llog -ldl
cp -f "$OUT/lib/libacos_binder_driver.so" "$SCRATCH/build/"

echo "--- peer (cargo, real ServiceHost) ---"
( cd "$ACOS_ROOT" && cargo build --release -p acos-binder-peer )
cp -f "$ACOS_ROOT/target/release/acos-binder-peer" "$SCRATCH/build/acos-binder-peer"

echo "--- java ---"
rm -rf "$SCRATCH/classes"; mkdir -p "$SCRATCH/classes"
javac -nowarn -source 11 -target 11 -cp "$SDKJAR" -d "$SCRATCH/classes" \
      $(find "$CRATE/tests/java" -name '*.java') 2>&1 |
    grep -v "bootstrap class path\|^1 warning\|warning: \[options\]" || true

rm -rf "$OUT/dex/binderpeer"; mkdir -p "$OUT/dex/binderpeer"
"$D8" --lib "$SDKJAR" --min-api 34 --output "$OUT/dex/binderpeer" \
      $(find "$SCRATCH/classes" -name '*.class') 2>&1 | grep -v "^Warning" || true
ls -la "$OUT/dex/binderpeer/classes.dex" "$OUT/lib/libacos_binder_driver.so" "$SCRATCH/build/acos-binder-peer"
