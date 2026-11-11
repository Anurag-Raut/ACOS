#!/usr/bin/env bash
# run.sh -- guest ladder under the mandatory guard.
#
#   scripts/run.sh hello | rn | all
set -uo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../../.." && pwd)"
HERE="$ROOT/research/art-feasibility/r2-native"
W="$ROOT/.cache/art-exp-r2"
mkdir -p "$W/logs"

HELLO_APK=/data/app/~~r2hello==/dev.acos.fixtures.hellojni-1==/base.apk
HELLO_LIB=/data/app/~~r2hello==/dev.acos.fixtures.hellojni-1==/lib/x86_64
RN_APK=/data/app/~~r2rn==/dev.acos.rnhello-1==/base.apk
RN_LIB=/data/app/~~r2rn==/dev.acos.rnhello-1==/lib/x86_64

run() { # <mode> <apk> <libdir> [preload]
    local mode="$1" apk="$2" lib="$3" pre="${4:-}" log="$W/logs/$1.log"
    echo "== $mode (log: $log)"
    R2_PRELOAD="$pre" R2_APP_LD_LIBRARY_PATH=/data/lib \
    "$ROOT/tools/guarded.sh" --mem 4G --time 300 -- \
        "$HERE/scripts/r2root.sh" dalvikvm -cp /data/dex/r2/classes.dex \
        dev.acos.r2.R2Main "$mode" "$apk" "$lib" /data/dex/r2/classes.dex 2>&1 | tee "$log"
    echo "== $mode exit=${PIPESTATUS[0]}"
}

case "${1:-all}" in
    hello)     run hello     "$HELLO_APK" "$HELLO_LIB" /data/lib/libr2shim.so ;;
    hello-raw) run hello-raw "$HELLO_APK" "$HELLO_LIB" ;;
    rn)        run rn        "$RN_APK"    "$RN_LIB"    /data/lib/libr2shim.so ;;
    all)
        run hello-raw "$HELLO_APK" "$HELLO_LIB"
        run hello     "$HELLO_APK" "$HELLO_LIB" /data/lib/libr2shim.so
        run rn        "$RN_APK"    "$RN_LIB"    /data/lib/libr2shim.so ;;
    *) echo "usage: run.sh hello|hello-raw|rn|all" >&2; exit 2 ;;
esac
