#!/usr/bin/env bash
# build.sh -- build the E4H probe (javac + d8) into the E1A mini-root.
# Reuses E2D's libbindershim.so (Nat.register) untouched: the dex goes to
# /data/dex/e4h/classes.dex and is run with /data/dex/e2d/classes.dex on the
# classpath so dev.acos.e2d.Nat is available.
set -eo pipefail
E4H="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
. "$E4H/../../a-dalvikvm/scripts/env.sh"

D8="$(ls -d "$HOME"/Android/Sdk/build-tools/*/d8 2>/dev/null | tail -1)"
SDKJAR="${ANDROID_JAR:-$HOME/Android/Sdk/platforms/android-34/android.jar}"
OUT="${ART_DATA:-$ANDROID_DATA}"; mkdir -p "$OUT/dex/e4h" "$OUT/out"
BUILD="$ACOS_ROOT/.cache/art-exp-h/build"; mkdir -p "$BUILD"

rm -rf "$BUILD/classes"; mkdir -p "$BUILD/classes"
# dev.acos.e2d.Nat is compiled from source (the JNI symbol names are bound to
# that class); its .class is dropped again so it is not duplicated in our dex.
javac -nowarn -source 11 -target 11 -cp "$SDKJAR" -d "$BUILD/classes" \
      -sourcepath "$E4H/../java:$E4H/../../d-framework/java" \
      $(find "$E4H/../java" -name '*.java') 2>&1 |
    grep -v "bootstrap class path\|^1 warning\|warning: \[options\]" || true
rm -rf "$BUILD/classes/dev/acos/e2d"

rm -rf "$OUT/dex/e4h"; mkdir -p "$OUT/dex/e4h"
"$D8" --lib "$SDKJAR" --min-api 34 --output "$OUT/dex/e4h" \
      $(find "$BUILD/classes" -name '*.class') 2>&1 | grep -v "^Warning" || true
ls -la "$OUT/dex/e4h/classes.dex"
cp -f "$ACOS_ROOT/fixtures/build/apps/counter.apk" "$OUT/apk/counter.apk"
ls -la "$OUT/apk/counter.apk"
