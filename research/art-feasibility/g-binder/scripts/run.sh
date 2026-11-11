#!/usr/bin/env bash
# run.sh -- one rung of the E3G ladder (or all of them).
#
#   scripts/run.sh all            # smgr, echo, fd, latency, reverse
#   scripts/run.sh echo
#   E3G_DEBUG=1 scripts/run.sh all  # driver trace on stderr
#
# Starts the Rust peer (servicemanager + echo service) on /data/e3g.sock, then
# runs the test dex on dalvikvm64 inside the E1A/E2D mini-root.
set -uo pipefail
E3G_HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
. "$E3G_HERE/../../a-dalvikvm/scripts/env.sh"

STEP="${1:-all}"
DATA="${ART_DATA:-$ANDROID_DATA}"
SCRATCH="$ACOS_ROOT/.cache/art-exp-g"
LOGS="$SCRATCH/logs"; mkdir -p "$LOGS"
DEX=/data/dex/e3g/classes.dex
SOCK="$DATA/e3g.sock"

rm -f "$SOCK"
"$SCRATCH/build/e3g-peer" "$SOCK" >"$LOGS/peer.log" 2>&1 &
PEER_PID=$!
trap 'kill $PEER_PID 2>/dev/null' EXIT
for i in $(seq 1 50); do [ -S "$SOCK" ] && break; sleep 0.05; done

unset LD_LIBRARY_PATH
export E3G_PRELOAD="${E3G_PRELOAD-/data/lib/libbindere3g.so}"
export ACOS_GUARD_TIME="${ACOS_GUARD_TIME:-120}"
"$ACOS_ROOT/tools/guarded.sh" --time "$ACOS_GUARD_TIME" -- \
    "$E3G_HERE/e3groot.sh" dalvikvm -cp "$DEX" dev.acos.e3g.Main "$STEP"
RC=$?
kill $PEER_PID 2>/dev/null
wait $PEER_PID 2>/dev/null
echo "--- peer log ($LOGS/peer.log) ---"
sed 's/^/  /' "$LOGS/peer.log"
exit $RC
