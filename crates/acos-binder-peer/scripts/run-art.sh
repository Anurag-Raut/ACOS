#!/usr/bin/env bash
# Run the P2d acceptance dex on dalvikvm64 in the E1A/E2D/E3G mini-root:
# stock libbinder + the userspace driver + the real ServiceHost peer.
#
#   scripts/run-art.sh            # under tools/guard/guarded.sh
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
CRATE="$(cd "$HERE/.." && pwd)"
. "$CRATE/../../research/art-feasibility/a-dalvikvm/scripts/env.sh"

DATA="${ART_DATA:-$ANDROID_DATA}"
SCRATCH="$ACOS_ROOT/.cache/art-exp-p2d"
LOGS="$SCRATCH/logs"; mkdir -p "$LOGS"
# e3groot.sh exports E3G_SOCK=/data/e3g.sock for the driver.
DEX=/data/dex/binderpeer/classes.dex
SOCK="$DATA/e3g.sock"
LOG="$LOGS/unsupported-$(date +%s).jsonl"

rm -f "$SOCK"
"$SCRATCH/build/acos-binder-peer" "$SOCK" "$LOG" >"$LOGS/peer.log" 2>&1 &
PEER_PID=$!
trap 'kill $PEER_PID 2>/dev/null' EXIT
for i in $(seq 1 50); do [ -S "$SOCK" ] && break; sleep 0.05; done

unset LD_LIBRARY_PATH
export E3G_PRELOAD=/data/lib/libacos_binder_driver.so
export ACOS_GUARD_TIME="${ACOS_GUARD_TIME:-180}"
"$ACOS_ROOT/tools/guard/guarded.sh" --time "$ACOS_GUARD_TIME" -- \
    "$ACOS_ROOT/research/art-feasibility/g-binder/scripts/e3groot.sh" \
    dalvikvm -cp "$DEX" dev.acos.binderpeer.Main
RC=$?
kill $PEER_PID 2>/dev/null
wait $PEER_PID 2>/dev/null
echo "--- peer log ($LOGS/peer.log) ---"
sed 's/^/  /' "$LOGS/peer.log"
echo "--- unsupported JSONL ($LOG) ---"
[ -f "$LOG" ] && sed 's/^/  /' "$LOG" || echo "  (none)"
exit $RC
