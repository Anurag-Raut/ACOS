#!/usr/bin/env bash
# W39 before/after: wall from exec to `activity-resumed`, N cold launches with a
# fresh ACOS_ART_DATA per launch, and the per-stage markers of each launch.
#
#   W39-measure.sh <before|after> [N]
#
# `before` = ACOS_ART_BUILD_CACHE=off (pre-W37 behaviour: every per-launch
# staging step runs again); `after` = the shared content-addressed cache.
set -u
ROOT=/home/Qui/Projects/acos
MODE=${1:-after}
N=${2:-20}
BIN=${BIN:-$ROOT/.cache/w39-target/release/acos-art}
APK=$ROOT/fixtures/build/apps/counter.apk
OUT=$ROOT/.cache/w39-bench/$MODE
rm -rf "$OUT"; mkdir -p "$OUT"
if [ "$MODE" = before ]; then
    export ACOS_ART_BUILD_CACHE=off
else
    export ACOS_ART_BUILD_CACHE=$ROOT/.cache/art/build-cache
fi
export ACOS_ART_STAGE_TIMING=1
run() { # run <tag>
    local tag=$1
    local d=$OUT/data-$tag
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
