#!/usr/bin/env bash
# run.sh -- one rung of the E2D ladder (or all of them).
#
#   scripts/run.sh all            # bundle, parcel, looper, binder, resources, activity
#   scripts/run.sh resources      # one rung
#   E2D_BINDER_FAKE=0 scripts/run.sh binder   # without the memfd binder stand-in
#
# Prerequisites: scripts/build.sh, and the E1A mini-root
# (research/art-feasibility/a-dalvikvm/scripts/artroot.sh) already brought up once.
set -uo pipefail
E2D_HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
. "$E2D_HERE/../../a-dalvikvm/scripts/env.sh"

STEP="${1:-all}"
DATA="${ART_DATA:-$ANDROID_DATA}"
DEX=/data/dex/e2d/classes.dex
PRELOAD="${E2D_PRELOAD-/data/lib/libbindershim.so}"

# A host LD_LIBRARY_PATH leaks into libnativeloader's classloader namespace and
# makes the (already failing) System.load path noisier; drop it.
unset LD_LIBRARY_PATH
export E2D_PRELOAD="$PRELOAD"
export E2D_BINDER_FAKE="${E2D_BINDER_FAKE:-1}"
export ACOS_GUARD_TIME="${ACOS_GUARD_TIME:-120}"

exec "$ACOS_ROOT/tools/guarded.sh" --time "$ACOS_GUARD_TIME" -- \
    "$E2D_HERE/e2droot.sh" dalvikvm -cp "$DEX" dev.acos.e2d.Main "$STEP"
