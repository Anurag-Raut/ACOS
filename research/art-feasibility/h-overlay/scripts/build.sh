#!/usr/bin/env bash
# build.sh -- build the H-overlay probe (javac + d8) into the E1A mini-root.
# Same toolchain as E4H/h-view: reuses E2D's libbindershim.so (dev.acos.e2d.Nat)
# untouched; the dex goes to /data/dex/hov/classes.dex and runs with
# /data/dex/e2d/classes.dex on the classpath.
set -eo pipefail
HOV="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
. "$HOV/../../a-dalvikvm/scripts/env.sh"

D8="$(ls -d "$HOME"/Android/Sdk/build-tools/*/d8 2>/dev/null | tail -1)"
SDKJAR="${ANDROID_JAR:-$HOME/Android/Sdk/platforms/android-34/android.jar}"
OUT="${ART_DATA:-$ANDROID_DATA}"; mkdir -p "$OUT/dex/hov" "$OUT/out"
BUILD="$ACOS_ROOT/.cache/art-exp-h/build-hov"; mkdir -p "$BUILD"

rm -rf "$BUILD/classes"; mkdir -p "$BUILD/classes"
# dev.acos.e2d.Nat is compiled from source (the JNI symbol names are bound to
# that class); its .class is dropped again so it is not duplicated in our dex.
javac -nowarn -source 11 -target 11 -cp "$SDKJAR" -d "$BUILD/classes" \
      -sourcepath "$HOV/../java:$HOV/../../d-framework/java" \
      $(find "$HOV/../java" -name '*.java') 2>&1 |
    grep -v "bootstrap class path\|^1 warning\|warning: \[options\]" || true
rm -rf "$BUILD/classes/dev/acos/e2d"

rm -rf "$OUT/dex/hov"; mkdir -p "$OUT/dex/hov"
"$D8" --lib "$SDKJAR" --min-api 34 --output "$OUT/dex/hov" \
      $(find "$BUILD/classes" -name '*.class') 2>&1 | grep -v "^Warning" || true
ls -la "$OUT/dex/hov/classes.dex"
mkdir -p "$OUT/apk"
cp -f "$ACOS_ROOT/fixtures/build/apps/counter.apk" "$OUT/apk/counter.apk"
ls -la "$OUT/apk/counter.apk"
