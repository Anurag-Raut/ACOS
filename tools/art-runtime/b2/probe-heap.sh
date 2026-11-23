#!/usr/bin/env bash
# B2: build and run the throwaway guest heap probe through the product's own
# ART path (same sysroot, same property table, same preloads).
#
#   tools/art-runtime/b2/probe-heap.sh [--garbage-mib N] [--heap VAR=VAL ...]
#
# Everything lands in .cache/b2/probe. No product code is touched.
set -euo pipefail
ROOT=/home/Qui/Projects/acos
OUT="$ROOT/.cache/b2/probe"
SDK="${ANDROID_SDK_ROOT:-$HOME/Android/Sdk}"
ANDROID_JAR="$SDK/platforms/android-34/android.jar"
D8="$SDK/build-tools/34.0.0/d8"
ART="$ROOT/target/release/acos-art"
SRC="$ROOT/research/b2/HeapProbe.java"

garbage=0
while [ $# -gt 0 ]; do
    case "$1" in
        --garbage-mib) garbage=$2; shift 2 ;;
        *) echo "usage: $0 [--garbage-mib N]" >&2; exit 2 ;;
    esac
done

mkdir -p "$OUT/classes"
[ -f "$ANDROID_JAR" ] || { echo "missing $ANDROID_JAR" >&2; exit 2; }
[ -x "$D8" ] || { echo "missing $D8" >&2; exit 2; }
javac -nowarn -source 11 -target 11 -cp "$ANDROID_JAR" -d "$OUT/classes" "$SRC"
"$D8" --lib "$ANDROID_JAR" --min-api 34 --output "$OUT" \
    "$OUT/classes/b2/HeapProbe.class"
ls -l "$OUT/classes.dex" >&2

export ACOS_ART_DATA="$ROOT/.cache/b2/probe/data"
mkdir -p "$ACOS_ART_DATA"
exec "$ROOT/tools/art-runtime/run-exclusive.sh" --task B2 -- \
    "$ROOT/tools/guard/guarded.sh" --mem 8G --time 900 -- \
    "$ART" run --dex "$OUT/classes.dex" --class b2.HeapProbe \
    --image none --timeout 300 -- --garbage-mib "$garbage"
