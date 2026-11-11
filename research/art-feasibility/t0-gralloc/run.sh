#!/usr/bin/env bash
# run.sh -- build + run the T0 gralloc interposer probe inside the ACOS bwrap
# mini-Android root.  Scratch (build outputs, /data bind source) lives under
# .cache/t0-gralloc/ ; nothing is written to /tmp and crates/ is untouched.
#
#   tools/guarded.sh --mem 4G --time 300 -- research/art-feasibility/t0-gralloc/run.sh [--control]
#
#   --control   run the probe WITHOUT the LD_PRELOAD interposer (expected: the
#               stock libui.so LOG_ALWAYS_FATALs in GraphicBufferMapper's ctor)
set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
SRC="$ROOT/research/art-feasibility/t0-gralloc"
SYS="${ACOS_ART_SYSROOT:-$ROOT/.cache/art/sysroot}"
LINKCFG="${ACOS_ART_DATA:-$ROOT/.cache/art/data}/linkerconfig"
WORK="$ROOT/.cache/t0-gralloc"
DATA="$WORK/data"
PRELOAD=1
[ "${1:-}" = "--control" ] && PRELOAD=0

NDK="${ANDROID_NDK_HOME:-${ANDROID_NDK_ROOT:-$HOME/Android/Sdk/ndk/27.1.12297006}}"
CC="$NDK/toolchains/llvm/prebuilt/linux-x86_64/bin/x86_64-linux-android34-clang"
[ -x "$CC" ] || { echo "run.sh: no NDK clang at $CC" >&2; exit 2; }
[ -d "$SYS/system/lib64" ] || { echo "run.sh: no sysroot at $SYS" >&2; exit 2; }

mkdir -p "$DATA"
"$CC" -O2 -fPIC -shared -Wall -Wno-unused-parameter \
    -o "$DATA/libacet0.so" "$SRC/gralloc_memfd_shim.c" || exit 3
"$CC" -O2 -Wall -Wno-unused-parameter \
    -o "$DATA/gbprobe" "$SRC/gbprobe.c" \
    -L"$SYS/system/lib64" -lui -Wl,-rpath-link,"$SYS/system/lib64" || exit 3

ARGS=(
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
# libc/libm/libdl/libc++/libui live in /system/lib64; the bionic linker itself
# (and every libc symbol it needs) lives in the runtime APEX -- /system/bin/linker64
# is a symlink to /apex/com.android.runtime/bin/linker64, so that APEX must be
# mounted or execvp fails with ENOENT (the first failure this probe hit).
for m in "$SYS/system/apex"/*; do
  name="$(basename "$m")"
  guest="/apex/$name"
  [ "$name" = "com.google.mainline.primary.libs" ] && guest=/apex/sharedlibs
  [ "$guest" = "/apex/com.android.runtime" ] && continue
  ARGS+=(--ro-bind "$m" "$guest")
done
if [ -f "$LINKCFG/ld.config.txt" ]; then
  ARGS+=(--ro-bind "$LINKCFG" /linkerconfig)
fi
if [ "$PRELOAD" -eq 1 ]; then
  ARGS+=(--setenv LD_PRELOAD /data/libacet0.so)
fi
ARGS+=(-- /data/gbprobe)

echo "== bwrap$( [ "$PRELOAD" -eq 1 ] && echo ' + LD_PRELOAD=/data/libacet0.so' || echo ' (control, no interposer)')"
bwrap "${ARGS[@]}"
echo "== exit=$?"
