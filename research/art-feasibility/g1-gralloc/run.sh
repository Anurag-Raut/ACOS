#!/usr/bin/env bash
# run.sh -- build + run the G1 gralloc probes inside the ACOS bwrap mini-Android
# root, with the *installed* shim (<sysroot>/shim/libacoshw.so, built by
# tools/art-sysroot.sh --shim) preloaded.
#
#   tools/guarded.sh --mem 4G --time 300 -- research/art-feasibility/g1-gralloc/run.sh [flatten|blast|all]
#
# Scratch (binaries, /data bind source) lives under .cache/g1/ ; nothing is
# written to /tmp and crates/ is untouched.
set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
SRC="$ROOT/research/art-feasibility/g1-gralloc"
SYS="${ACOS_ART_SYSROOT:-$ROOT/.cache/art/sysroot}"
LINKCFG="${ACOS_ART_DATA:-$ROOT/.cache/art/data}/linkerconfig"
WORK="$ROOT/.cache/g1"
DATA="$WORK/data"
WHICH="${1:-all}"

NDK="${ANDROID_NDK_HOME:-${ANDROID_NDK_ROOT:-$HOME/Android/Sdk/ndk/27.1.12297006}}"
CC="$NDK/toolchains/llvm/prebuilt/linux-x86_64/bin/x86_64-linux-android34-clang"
[ -x "$CC" ] || { echo "run.sh: no NDK clang at $CC" >&2; exit 2; }
[ -d "$SYS/system/lib64" ] || { echo "run.sh: no sysroot at $SYS" >&2; exit 2; }

# The shim under test is the one the sysroot installer produced (same source,
# same command line); rebuild into .cache/g1 only if it is missing.
SHIM="$SYS/shim/libacoshw.so"
mkdir -p "$DATA"
export TMPDIR="$WORK"
if [ ! -f "$SHIM" ]; then
    echo "run.sh: $SHIM missing, building it (tools/art-sysroot.sh --shim does this)"
    SHIM="$DATA/libacoshw.so"
    "$CC" -O2 -fPIC -shared -Wall -Wno-unused-parameter \
        -o "$SHIM" "$ROOT/crates/acos-art/shim/gralloc_shim.c" || exit 3
fi

build_probe() {
    local name="$1"
    "$CC" -O2 -Wall -Wno-unused-parameter -o "$DATA/$name" "$SRC/$name.c" \
        -L"$SYS/system/lib64" -lui -lgui -Wl,-rpath-link,"$SYS/system/lib64" || exit 3
}
build_probe probe_flatten
build_probe probe_blast
build_probe probe_mem

BASE=(
  --unshare-pid --unshare-net --unshare-ipc --unshare-uts --unshare-cgroup-try
  --die-with-parent --clearenv --cap-drop ALL
  --tmpfs /
  --ro-bind "$SYS/system" /system
  --dir /apex
  --ro-bind "$SYS/system/apex/com.android.runtime" /apex/com.android.runtime
  --dir /data
  --bind "$DATA" /data
  --dev /dev
  --proc /proc
  --ro-bind /etc/passwd /etc/passwd
)
for m in "$SYS/system/apex"/*; do
  name="$(basename "$m")"
  guest="/apex/$name"
  [ "$name" = "com.google.mainline.primary.libs" ] && guest=/apex/sharedlibs
  [ "$guest" = "/apex/com.android.runtime" ] && continue
  BASE+=(--ro-bind "$m" "$guest")
done
[ -f "$LINKCFG/ld.config.txt" ] && BASE+=(--ro-bind "$LINKCFG" /linkerconfig)
# /data/libacoshw.so so LD_PRELOAD can name it like the launcher does
cp -f "$SHIM" "$DATA/libacoshw.so"

# Surface::connect() -> BufferQueueProducer::connect() already walks a local
# BnProducerListener, but libbinder's ProcessState is still initialised on that
# path, and with no /dev/binder it LOG_ALWAYS_FATALs. The render shim already
# carries the memfd stand-in the real guest uses (ACOSART_BINDER_FAKE=1), so the
# blast probe preloads it too -- same mechanism, same order as `acos-art
# render-app`: libacosart.so (logd/property/binder-fake) then libacoshw.so.
RENDER_SHIM="${ACOSART_RENDER_SHIM:-$ROOT/.cache/art/data/app/libacosart.so}"

run_probe() {
    local p="$1"
    local pre="/data/libacoshw.so"
    local extra=()
    if [ "$p" = "probe_blast" ] && [ -f "$RENDER_SHIM" ]; then
        cp -f "$RENDER_SHIM" "$DATA/libacosart.so"
        pre="/data/libacosart.so:/data/libacoshw.so"
        extra=(--setenv ACOSART_LOAD : --setenv ACOSART_BINDER_FAKE 1)
    fi
    echo "== bwrap + LD_PRELOAD=$pre : $p"
    bwrap "${BASE[@]}" --setenv LD_PRELOAD "$pre" "${extra[@]}" \
          --setenv ACOSART_HW_VERBOSE "${ACOSART_HW_VERBOSE:-0}" \
          -- "/data/$p"
    echo "== exit=$?"
}

case "$WHICH" in
    flatten) run_probe probe_flatten ;;
    blast)   run_probe probe_blast ;;
    mem)     run_probe probe_mem ;;
    all)     run_probe probe_flatten; run_probe probe_blast; run_probe probe_mem ;;
    *) echo "usage: run.sh [flatten|blast|mem|all]" >&2; exit 2 ;;
esac
