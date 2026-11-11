#!/usr/bin/env bash
# repro.sh -- W3: the leash wall, the cascade replay, and the fixed launch.
#
#   scripts/repro.sh cascade   # replay the image's readers over the real reply
#   scripts/repro.sh launch    # the launch (one ART guest, guarded, .cache scratch)
#   scripts/repro.sh render    # counter preview md5 (must stay 9ec17f05...)
#   scripts/repro.sh attach    # optional: host gdb attach to the guest (see README)
#
# Everything goes through tools/guarded.sh --mem 4G --time 300 and keeps scratch
# under .cache/w3/ (never /tmp).  The launch uses a *scratch* data root so the
# shared .cache/art/data is untouched.
set -uo pipefail
W3="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ROOT="$(cd "$W3/../../.." && pwd)"
cd "$ROOT"
DATA=${ACOS_ART_DATA:-$ROOT/.cache/w3/data}
export ACOS_ART_DATA=$DATA

case "${1:-cascade}" in
    cascade) python3 "$W3/scripts/cascade.py" "$W3/artifacts/w3-frame-add.hex" ;;
    launch)
        out="$ROOT/.cache/w3/launch"
        rm -rf "$out"
        tools/guarded.sh --mem 4G --time 300 -- ./target/debug/acos-art launch \
            --apk fixtures/build/apps/counter.apk --until activity-resumed \
            --out "$out" 2>&1 | grep -E "wall=|outcome|InputEventReceiver|bad array"
        ;;
    render)
        out="$ROOT/.cache/w3/counter"
        rm -rf "$out"
        tools/guarded.sh --mem 4G --time 300 -- ./target/debug/acos-art render-app \
            --apk fixtures/build/apps/counter.apk --out "$out" 2>&1 | grep md5
        ;;
    attach)
        # yama ptrace_scope=1: the guest is not our descendant (systemd-run
        # --scope hands it to systemd), so it must allow attaching itself.
        # The scratch data root's shim is rebuilt with ptracer.c appended:
        mkdir -p "$DATA/shim"
        clang -nostdlib -ffreestanding -fno-builtin -fPIC -shared -O2 -Wall \
            -o "$DATA/shim/libartshim.so" \
            "$DATA/shim/artshim.c" "$W3/scripts/ptracer.c"
        echo "shim rebuilt with PR_SET_PTRACER; run the launch, find the guest"
        echo "with scripts/find-guest.py, then: gdb -p PID -x $W3/scripts/attach.gdb"
        ;;
    *) echo "usage: repro.sh cascade|launch|render|attach" >&2; exit 2 ;;
esac
