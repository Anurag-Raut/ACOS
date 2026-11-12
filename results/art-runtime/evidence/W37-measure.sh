#!/usr/bin/env bash
# W37 before/after: wall from exec to activity-resumed, 20 cold launches.
set -u
ROOT=/home/Qui/Projects/acos
MODE=$1
N=${2:-20}
BIN=$ROOT/.cache/w37-target/release/acos-art
APK=$ROOT/fixtures/build/apps/counter.apk
OUT=$ROOT/.cache/w37-bench/$MODE
rm -rf "$OUT"; mkdir -p "$OUT"
if [ "$MODE" = before ]; then
    export ACOS_ART_BUILD_CACHE=off
else
    export ACOS_ART_BUILD_CACHE=$ROOT/.cache/art/build-cache
fi
run() { # run <tag>
    local tag=$1 d=$OUT/data-$1
    rm -rf "$d"; mkdir -p "$d"
    local s e rc
    s=$(date +%s%N)
    ACOS_ART_DATA=$d "$BIN" launch --apk "$APK" --until activity-resumed --out "$OUT/out-$tag" \
        >"$OUT/stdout-$tag.txt" 2>"$OUT/stderr-$tag.txt"
    rc=$?
    e=$(date +%s%N)
    echo "$tag $rc $(( (e - s) / 1000000 ))" >>"$OUT/samples.txt"
}
if [ "$MODE" = after ]; then
    run warmup            # builds every cache entry once (cold cache)
fi
for i in $(seq 1 "$N"); do run "$i"; done
