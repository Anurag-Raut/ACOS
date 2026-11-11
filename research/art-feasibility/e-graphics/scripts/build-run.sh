#!/usr/bin/env bash
# build-run.sh -- rebuild and run the E2 probe: real ART + real framework
# android.graphics + real libhwui Skia CPU canvas, no Android boot.
set -euo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
E2="$HERE/../e2"
ACOS_ROOT="$(cd "$HERE/../../../.." && pwd)"
NDK="${ANDROID_NDK:-$HOME/Android/Sdk/ndk/27.1.12297006}"
CC="$NDK/toolchains/llvm/prebuilt/linux-x86_64/bin/x86_64-linux-android34-clang"
AJ="${ANDROID_JAR:-$HOME/Android/Sdk/platforms/android-34/android.jar}"
D8="$(ls -d "$HOME"/Android/Sdk/build-tools/*/d8 2>/dev/null | tail -1)"
DATA="$ACOS_ROOT/.cache/art-exp/data"

mkdir -p "$DATA/lib" "$DATA/dex/e2"
"$CC" -O2 -shared -fPIC -I"$E2" -o "$DATA/lib/libacosgraphicsbridge.so" "$E2/bridge.c" -ldl -llog
rm -rf "$E2/classes"; mkdir -p "$E2/classes"
javac -nowarn -source 8 -target 8 -bootclasspath "$AJ" -d "$E2/classes" "$E2"/java/dev/acos/e2/*.java
"$D8" --min-api 34 --lib "$AJ" --output "$E2/classes" $(find "$E2/classes" -name '*.class')
cp -f "$E2/classes/classes.dex" "$DATA/dex/e2/classes.dex"

"$HERE/public-libs.sh"          # namespace permission for libhwui/libandroid_runtime
exec "$ACOS_ROOT/tools/guarded.sh" --mem 6G --time 240 -- \
    "$ACOS_ROOT/research/art-feasibility/a-dalvikvm/scripts/artroot.sh" \
    dalvikvm -cp /system/framework/framework.jar:/data/dex/e2/classes.dex dev.acos.e2.Main
