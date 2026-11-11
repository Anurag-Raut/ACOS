#!/usr/bin/env bash
# run.sh -- run the E4H probe under the guard.
#
#   scripts/run.sh            # the whole ladder
#   scripts/run.sh gfx        # one rung
#   E4H_PRELOAD=0 scripts/run.sh all     # without the substrate shim (control)
#
# E2D_LOAD pulls libhwui.so into the default linker namespace *before* any Java
# runs (that is what makes the register function reachable; see E2D/E2E).
set -uo pipefail
E4H="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
. "$E4H/../../a-dalvikvm/scripts/env.sh"

ARGSTEP="${1:-all}"
DATA="${ART_DATA:-$ANDROID_DATA}"
unset LD_LIBRARY_PATH
export E2D_LOAD="${E2D_LOAD:-libandroid_runtime.so:libhwui.so}"
export E2D_BINDER_FAKE="${E2D_BINDER_FAKE:-1}"
if [ "${E4H_PRELOAD:-1}" = "0" ]; then
    export E2D_PRELOAD=""
else
    export E2D_PRELOAD="${E2D_PRELOAD-/data/lib/libbindershim.so}"
fi
export ACOS_GUARD_TIME="${ACOS_GUARD_TIME:-180}"

LOG="$ACOS_ROOT/.cache/art-exp-h/logs"
mkdir -p "$LOG" "$DATA/out"
set -o pipefail
"$ACOS_ROOT/tools/guarded.sh" --mem 6G --time "$ACOS_GUARD_TIME" -- \
    "$E4H/hroot.sh" dalvikvm \
    -cp "/system/framework/framework.jar:/data/dex/e2d/classes.dex:/data/dex/e4h/classes.dex" \
    -De4h.apk=/data/apk/counter.apk \
    -De4h.pkg=dev.acos.fixtures.counter \
    -De4h.out=/data/out \
    dev.acos.e4h.Main "$ARGSTEP" 2>&1 | tee "$LOG/$ARGSTEP.log"
