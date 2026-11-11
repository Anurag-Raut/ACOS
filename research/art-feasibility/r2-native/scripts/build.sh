#!/usr/bin/env bash
# build.sh -- javac + d8 the R2 guest, and lay out the fixtures the way an
# installed package looks on a device:
#
#   <DATA>/app/~~r2hello==/dev.acos.fixtures.hellojni-1==/base.apk
#   <DATA>/app/~~r2hello==/dev.acos.fixtures.hellojni-1==/lib/x86_64/*.so
#   <DATA>/app/~~r2rn==/dev.acos.rnhello-1==/base.apk
#   <DATA>/app/~~r2rn==/dev.acos.rnhello-1==/lib/x86_64/*.so
#   <DATA>/dex/r2/classes.dex
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../../.." && pwd)"   # repo root
HERE="$ROOT/research/art-feasibility/r2-native"
SDK="${ANDROID_SDK_ROOT:-$HOME/Android/Sdk}"
BT="$SDK/build-tools/35.0.0"
JAR="$SDK/platforms/android-34/android.jar"
W="$ROOT/.cache/art-exp-r2"
DATA="$W/data"
JAVA="$W/java"

for t in "$JAR" "$BT/d8"; do [ -e "$t" ] || { echo "build: missing $t" >&2; exit 1; }; done

rm -rf "$JAVA" && mkdir -p "$JAVA/classes" "$JAVA/dex" "$DATA/dex/r2"
find "$HERE/java" -name '*.java' >"$JAVA/sources.txt"
javac -nowarn -source 8 -target 8 -bootclasspath "$JAR" -d "$JAVA/classes" \
      @"$JAVA/sources.txt" 2>&1 | grep -v 'bootstrap class path' || true
"$BT/d8" --min-api 34 --lib "$JAR" --output "$JAVA/dex" $(find "$JAVA/classes" -name '*.class')
cp -f "$JAVA/dex/classes.dex" "$DATA/dex/r2/classes.dex"

layout() { # <tag> <slug> <pkg> <apk>
    local tag="$1" slug="$2" pkg="$3" apk="$4"
    local d="$DATA/app/~~$slug==/$pkg-1=="
    rm -rf "$d" && mkdir -p "$d/lib/x86_64"
    cp -f "$apk" "$d/base.apk"
    unzip -q -o "$apk" 'lib/x86_64/*' -d "$d.tmp"
    cp -f "$d.tmp/lib/x86_64/"*.so "$d/lib/x86_64/"
    rm -rf "$d.tmp"
    echo "layout $tag -> $d/lib/x86_64: $(ls "$d/lib/x86_64" | tr '\n' ' ')"
}

layout hello r2hello dev.acos.fixtures.hellojni "$ROOT/fixtures/build/apps/hello-jni.apk"
layout rn    r2rn    dev.acos.rnhello         "$ROOT/fixtures/build/apps/rn-hello.apk"

# The one framework shim R2 needs (see native/r2shim.c).
NDKBIN="$SDK/ndk/27.1.12297006/toolchains/llvm/prebuilt/linux-x86_64/bin"
[ -x "$NDKBIN/x86_64-linux-android34-clang" ] || { echo "build: no NDK clang at $NDKBIN" >&2; exit 1; }
mkdir -p "$DATA/lib"
"$NDKBIN/x86_64-linux-android34-clang" -O2 -fPIC -shared -Wall \
    -o "$DATA/lib/libr2shim.so" "$HERE/native/r2shim.c"
echo "shim: $(stat -c %s "$DATA/lib/libr2shim.so") bytes, NEEDED=$(readelf -d "$DATA/lib/libr2shim.so" | sed -n 's/.*Shared library: \[\(.*\)\]/\1/p' | tr '\n' ' ')"
echo "guest dex: $(stat -c %s "$DATA/dex/r2/classes.dex") bytes"
