#!/usr/bin/env bash
# Z2: build the prototype (Java -> dex, C -> JNI shim). No product code involved.
#   research/zygote-proto/build.sh
# Scratch stays on the repo volume under .cache/z2-build/, never /tmp.
set -euo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
CACHE="${ACOS_CACHE:-/home/Qui/Projects/acos/.cache}"
SDK="${ANDROID_HOME:-$HOME/Android/Sdk}"
AJAR="$SDK/platforms/android-34/android.jar"
D8="$(ls -d "$SDK"/build-tools/*/d8 2>/dev/null | sort | tail -1)"
OUT="$CACHE/z2-build"
rm -rf "$OUT/classes" "$OUT/dex"
mkdir -p "$OUT/classes" "$OUT/dex"

[ -f "$AJAR" ] || { echo "missing $AJAR" >&2; exit 1; }
[ -n "$D8" ] || { echo "missing d8 in $SDK/build-tools" >&2; exit 1; }

mapfile -t SRC < <(find "$HERE/java" -name '*.java' | sort)
# -nowarn/-source 11: the same flags crates/acos-art/src/launch.rs uses.
javac -nowarn -source 11 -target 11 -cp "$AJAR" -d "$OUT/classes" "${SRC[@]}"
"$D8" --min-api 34 --lib "$AJAR" --output "$OUT/dex" $(find "$OUT/classes" -name '*.class') >/dev/null

# freestanding JNI shim, exactly the recipe of crates/acos-art/shim/artshim.c
clang -nostdlib -ffreestanding -fno-builtin -fno-stack-protector -fPIC -shared -O2 -Wall \
    -o "$OUT/libz2fork.so" "$HERE/shim/z2fork.c"

echo "built $OUT/dex/classes.dex and $OUT/libz2fork.so"
