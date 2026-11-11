#!/usr/bin/env bash
# Debug harness for the P2f reverse-delivery crash: run the P2d acceptance dex
# inside the E1A/E2D/E3G mini-root *under host gdb*.
#
# e3groot.sh execs bwrap with a fixed argument list, so a debugger cannot be
# injected through it.  This script rebuilds the same mount namespace (copied
# from research/art-feasibility/g-binder/scripts/e3groot.sh, minus the
# one-time file generation) and additionally binds the host root read-only at
# /hostroot so a *host* gdb (glibc) can run next to the bionic ART.
#
#   crates/acos-binder-peer/scripts/dbg-art.sh            # batch: run + bt
#   GDB_TTY=1 crates/acos-binder-peer/scripts/dbg-art.sh  # interactive gdb
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
CRATE="$(cd "$HERE/.." && pwd)"
. "$CRATE/../../research/art-feasibility/a-dalvikvm/scripts/env.sh"

S="$ART_SYSROOT"
DATA="${ART_DATA:-$ACOS_ROOT/.cache/art-exp/data}"
LC="$DATA/linkerconfig"
SCRATCH="$ACOS_ROOT/.cache/art-exp-p2d"
LOGS="$SCRATCH/logs"
mkdir -p "$LOGS" "$SCRATCH/build"
unset LD_LIBRARY_PATH   # ART builds its namespace search path from it (E3G run-art.sh)
DEX=/data/dex/binderpeer/classes.dex
SOCK="$DATA/e3g.sock"

devpath() { printf '%s' "$1" | sed -e "s#$S/system/apex/#/apex/#g" -e "s#$S##g"; }

BWRAP_ARGS=(
  --tmpfs /
  --ro-bind "$S/system" /system
  --dir /apex
  --ro-bind "$DATA/apex-info-list.xml" /apex/apex-info-list.xml
)
for d in "$S/system/apex"/*; do
    [ -d "$d" ] || continue
    m="$(basename "$d")"
    case "$m" in
        com.google.mainline.primary.libs) BWRAP_ARGS+=(--ro-bind "$d" /apex/sharedlibs) ;;
        *) BWRAP_ARGS+=(--ro-bind "$d" "/apex/$m") ;;
    esac
done
BWRAP_ARGS+=(
  --bind "$LC" /linkerconfig
  --bind "$DATA" /data
  --dev /dev
  --proc /proc
  --ro-bind /sys /sys
  --dir /vendor --dir /product --dir /system_ext
  --ro-bind /etc/passwd /etc/passwd
  # host gdb + its glibc (Android does not use /usr, /lib64)
  --ro-bind /usr /usr
  --ro-bind /etc/ld.so.cache /etc/ld.so.cache
  --ro-bind /usr/lib/ld-linux-x86-64.so.2 /lib64/ld-linux-x86-64.so.2
)

[ -f "$LC/ld.config.txt" ] || { echo "dbg-art: run scripts/run-art.sh once first" >&2; exit 2; }

rm -f "$SOCK"
if ! pgrep -f "acos-binder-peer $SOCK" >/dev/null 2>&1; then
    "$SCRATCH/build/acos-binder-peer" "$SOCK" "$LOGS/unsupported-dbg.jsonl" \
        >"$LOGS/peer-dbg.log" 2>&1 &
    PEER_PID=$!
    trap 'kill $PEER_PID 2>/dev/null' EXIT
    for _ in $(seq 1 50); do [ -S "$SOCK" ] && break; sleep 0.05; done
fi

ENVV=(
  --setenv ANDROID_ROOT /system
  --setenv ANDROID_RUNTIME_ROOT /apex/com.android.runtime
  --setenv ANDROID_ART_ROOT /apex/com.android.art
  --setenv ANDROID_I18N_ROOT /apex/com.android.i18n
  --setenv ANDROID_TZDATA_ROOT /apex/com.google.android.tzdata5
  --setenv ANDROID_DATA /data
  --setenv BOOTCLASSPATH "$(devpath "$BOOTCLASSPATH")"
  --setenv DEX2OATBOOTCLASSPATH "$(devpath "$DEX2OATBOOTCLASSPATH")"
  --setenv E3G_SOCK /data/e3g.sock
  --setenv HOME /data
)
[ -n "${E3G_DEBUG:-}" ] && ENVV+=(--setenv E3G_DEBUG "$E3G_DEBUG")

PRELOAD="/system/lib64/liblogshim.so:${E3G_PRELOAD:-/data/lib/libacos_binder_driver.so}"
[ -n "${NO_GDB:-}" ] && ENVV+=(--setenv LD_PRELOAD "$PRELOAD")

GDB_CMDS=(
  -ex 'set pagination off'
  -ex "set env LD_PRELOAD $PRELOAD"
  -ex 'set debuginfod enabled off'
  -ex 'set startup-with-shell off'
  -ex 'set confirm off'
  -ex 'handle SIGPIPE nostop noprint pass'
  -ex 'run'
  -ex 'echo \n===== STOPPED =====\n'
  -ex 'info registers rip rsp rbp rax rbx rcx rdx rsi rdi'
  -ex 'x/12i $rip-24'
  -ex 'bt 40'
  -ex 'thread apply all bt 8'
  -ex 'echo \n===== MAPS =====\n'
  -ex 'info proc mappings'
  -ex 'kill'
)
[ -n "${GDB_TTY:-}" ] && GDB_CMDS=()

DBG=/system/apex/com.android.art/bin/dalvikvm64
if [ -n "${NO_GDB:-}" ]; then
    exec bwrap "${BWRAP_ARGS[@]}" "${ENVV[@]}" -- "$DBG" -cp "$DEX" dev.acos.binderpeer.Main
fi
exec bwrap "${BWRAP_ARGS[@]}" "${ENVV[@]}" -- \
    /usr/bin/gdb -q -nx "${GDB_CMDS[@]}" --args "$DBG" -cp "$DEX" dev.acos.binderpeer.Main
