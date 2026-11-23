#!/usr/bin/env bash
# g1-c1-run.sh -- G1 item 2: stock libgui BufferQueue producer (inside the ACOS
# bwrap mini-Android root) with C1's `surface_client` as the buffer consumer.
#
#   tools/art-runtime/g1-c1-run.sh
#
# Starts the C1 harness (research/art-feasibility/g1-gralloc/c1bridge, built
# with CARGO_TARGET_DIR=.cache/g1/c1target), then runs probe_blast in the ART
# sandbox with the bridge socket bound into /data/g1.sock. Serial: one ART
# process at a time; scratch under .cache/g1/ (never /tmp).
set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
WORK="$ROOT/.cache/g1"
SOCK="$WORK/c1.sock"
BIN="$WORK/c1target/release/g1-c1bridge"
LOG="$WORK/c1bridge.log"
mkdir -p "$WORK"
rm -f "$SOCK"
[ -x "$BIN" ] || { echo "g1-c1-run: build c1bridge first (cargo build --release in research/art-feasibility/g1-gralloc/c1bridge with CARGO_TARGET_DIR=$WORK/c1target)" >&2; exit 2; }

# The harness is the compositor-client host; it is as guarded as the guest run.
"$ROOT/tools/guard/guarded.sh" --mem 2G --time 300 -- "$BIN" "$SOCK" >"$LOG" 2>&1 &
HARNESS=$!
for _ in $(seq 1 100); do
    [ -S "$SOCK" ] && break
    sleep 0.05
done
[ -S "$SOCK" ] || { echo "g1-c1-run: harness never listened ($LOG)" >&2; kill "$HARNESS" 2>/dev/null; exit 2; }

G1_C1_SOCK="$SOCK" "$ROOT/tools/guard/guarded.sh" --mem 4G --time 240 -- \
    "$ROOT/research/art-feasibility/g1-gralloc/run.sh" blast
RC=$?
# the harness exits on EOF when the probe closes the socket
wait "$HARNESS" 2>/dev/null
echo "== harness log (tail)"
tail -6 "$LOG"
echo "== probe exit=$RC"
exit "$RC"
