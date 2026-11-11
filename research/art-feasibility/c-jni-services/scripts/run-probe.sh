#!/usr/bin/env bash
# run-probe.sh -- run the Java+JNI probe under *real ART* (dalvikvm64 inside
# the mini-Android root built by the A experiment).
#
#   scripts/run-probe.sh                       # full test + bench
#   scripts/run-probe.sh -Dprobe.mode=fork -Dfork.n=20
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
EXPDIR="$(cd "$HERE/.." && pwd)"
ACOS_ROOT="$(cd "$EXPDIR/../../.." && pwd)"
SVC="$ACOS_ROOT/.cache/art-exp-c/bin/acos-probe-service"
SOCK="$ACOS_ROOT/.cache/art-exp/data/service.sock"

# stand-in ServiceHost (Rust, uses the real acos-parcel codec)
SVC_PID=""
if [ -x "$SVC" ]; then
    "$SVC" "$SOCK" >"$ACOS_ROOT/.cache/art-exp-c/service.log" 2>&1 &
    SVC_PID=$!
    trap '[ -n "$SVC_PID" ] && kill $SVC_PID 2>/dev/null; rm -f "$SOCK"' EXIT
    for _ in $(seq 50); do [ -S "$SOCK" ] && break; sleep 0.05; done
fi

exec "$ACOS_ROOT/research/art-feasibility/a-dalvikvm/scripts/artroot.sh" \
    dalvikvm -cp /data/dex/artjni/classes.dex \
    -Dprobe.lib=/data/lib/libacosjniprobe.so "$@" dev.acos.artprobe.Main
