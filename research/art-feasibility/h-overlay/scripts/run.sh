#!/usr/bin/env bash
# run.sh -- run the H-overlay probe under the guard.
#
#   scripts/run.sh all                 # with the static RRO applied (idmap)
#   HOV_OVERLAY= scripts/run.sh all     # same harness, no overlay (control)
#   scripts/run.sh all --nocaps         # (E4H's forced-off variant still works)
#
# E2D_LOAD pulls libhwui.so into the default linker namespace *before* any Java
# runs (that is what makes the register function reachable; see E2D/E4E).
set -uo pipefail
HOV="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
. "$HOV/../../a-dalvikvm/scripts/env.sh"

ARGSTEP="${1:-all}"
DATA="${ART_DATA:-$ANDROID_DATA}"
unset LD_LIBRARY_PATH
export E2D_LOAD="${E2D_LOAD:-libandroid_runtime.so:libhwui.so}"
export E2D_BINDER_FAKE="${E2D_BINDER_FAKE:-1}"
if [ "${HOV_PRELOAD:-1}" = "0" ]; then
    export E2D_PRELOAD=""
else
    export E2D_PRELOAD="${E2D_PRELOAD-/data/lib/libbindershim.so}"
fi
export ACOS_GUARD_TIME="${ACOS_GUARD_TIME:-180}"

# Default: every static, android-target RRO the image ships (alphabetical =
# the order the framework applies them; later overlays win a clash).
D=/data/overlay
IDMAP="${HOV_OVERLAY-$D/framework-res__auto_generated_rro_product.idmap,$D/GoogleConfigOverlay.idmap,$D/GoogleWebViewOverlay.idmap,$D/LargeScreenConfigOverlay.idmap,$D/PixelConfigOverlayCommon.idmap}"
TARGET="${HOV_TARGET:-/system/framework/framework-res.apk}"

LOG="$ACOS_ROOT/.cache/art-exp-h/logs"
mkdir -p "$LOG" "$DATA/out"
set -o pipefail
"$ACOS_ROOT/tools/guarded.sh" --mem 6G --time "$ACOS_GUARD_TIME" -- \
    "$HOV/hroot.sh" dalvikvm \
    -cp "/system/framework/framework.jar:/data/dex/e2d/classes.dex:/data/dex/hov/classes.dex" \
    -Dhov.apk=/data/apk/counter.apk \
    -Dhov.pkg=dev.acos.fixtures.counter \
    -Dhov.out=/data/out \
    -Dhov.overlay="$IDMAP" \
    -Dhov.target="$TARGET" \
    -Dhov.loadflags="${HOV_LOADFLAGS:-}" \
    dev.acos.hov.Main "$ARGSTEP" 2>&1 | tee "$LOG/hov-$ARGSTEP.log"
