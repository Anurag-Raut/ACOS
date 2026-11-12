#!/usr/bin/env bash
# B2: build an **uncompressed** boot image with the sysroot's own stock
# `dex2oat64`, without touching product code.
#
# `acos-art build-image` hardcodes `--image-format=lz4`; this script reproduces
# the same dex2oat invocation (same sysroot, same device BOOTCLASSPATH, same
# `--base=0x70000000`) in a bwrap mini-Android root of the same shape the
# product's sandbox uses, and only swaps the image format. Nothing is
# downloaded; every input is the already-extracted `.cache/art/sysroot`.
#
#   tools/art-runtime/b2/build-uncompressed.sh [--out DIR] [--format FMT]
#
# Output: DIR/x86_64/{boot.art,boot.oat,boot.vdex,..} (named boot image).
set -euo pipefail
ROOT=/home/Qui/Projects/acos
S="$ROOT/.cache/art/sysroot"
OUT="$ROOT/.cache/b2/image"
FMT=uncompressed
LOGSHIM="$ROOT/.cache/art-exp/shim/liblogshim.so"

while [ $# -gt 0 ]; do
    case "$1" in
        --out) OUT=$2; shift 2 ;;
        --format) FMT=$2; shift 2 ;;
        *) echo "usage: $0 [--out DIR] [--format lz4|uncompressed]" >&2; exit 2 ;;
    esac
done

WORK="$ROOT/.cache/b2/artroot"
mkdir -p "$WORK" "$OUT/x86_64"
[ -d "$S/system" ] || { echo "missing sysroot $S" >&2; exit 2; }
[ -x "$S/system/apex/com.android.art/bin/dex2oat64" ] || { echo "no dex2oat64" >&2; exit 2; }

# Reuse the product data dir's linker config + apex info list (both are plain
# sysroot-derived files) but never its dalvik-cache or run dirs.
if [ ! -f "$WORK/linkerconfig/ld.config.txt" ]; then
    mkdir -p "$WORK/linkerconfig"
    cp -f "$ROOT/.cache/art/data/apex-info-list.xml" "$WORK/" 2>/dev/null || true
    cp -f "$ROOT/.cache/art/data/linkerconfig/"* "$WORK/linkerconfig/" 2>/dev/null || true
fi

# Device BOOTCLASSPATH: exactly the classpath the product serves
# (`.cache/art/data/system/environ/classpath`), device paths only.
CP_FILE="$ROOT/.cache/art/data/system/environ/classpath"
BOOTCP=$(sed -n 's/^export BOOTCLASSPATH //p' "$CP_FILE")
[ -n "$BOOTCP" ] || { echo "no BOOTCLASSPATH in $CP_FILE" >&2; exit 2; }

# bwrap mounts: /system read-only, every APEX at its device mount point, a
# writable /data, the derived /linkerconfig. Same shape as artroot.sh.
BWRAP=(
  --tmpfs /
  --ro-bind "$S/system" /system
  --dir /apex
  --ro-bind "$WORK/apex-info-list.xml" /apex/apex-info-list.xml
)
for d in "$S/system/apex"/*; do
    [ -d "$d" ] || continue
    m=$(basename "$d")
    # Same module-name mapping the product sandbox uses: the Google-signed
    # modules are mounted under their module name, not their APEX payload dir
    # (`com.google.android.configinfrastructure` -> `com.android.configinfrastructure`,
    # because BOOTCLASSPATH names /apex/com.android.configinfrastructure/...).
    case "$m" in
        com.google.mainline.primary.libs) BWRAP+=(--ro-bind "$d" /apex/sharedlibs) ;;
        com.google.*) BWRAP+=(--ro-bind "$d" "/apex/com.android.${m#com.google.android.}") ;;
        *) BWRAP+=(--ro-bind "$d" "/apex/$m") ;;
    esac
done
BWRAP+=(
  --bind "$WORK/linkerconfig" /linkerconfig
  --bind "$WORK" /data
  --bind "$OUT" /data/img
  --dev /dev --proc /proc --ro-bind /sys /sys
  --dir /vendor --dir /product --dir /system_ext
  --ro-bind /etc/passwd /etc/passwd
  --clearenv
  --setenv ANDROID_ROOT /system
  --setenv ANDROID_RUNTIME_ROOT /apex/com.android.runtime
  --setenv ANDROID_ART_ROOT /apex/com.android.art
  --setenv ANDROID_I18N_ROOT /apex/com.android.i18n
  --setenv ANDROID_TZDATA_ROOT /apex/com.google.android.tzdata5
  --setenv ANDROID_DATA /data
  --setenv BOOTCLASSPATH "$BOOTCP"
  --setenv DEX2OATBOOTCLASSPATH "$BOOTCP"
  --setenv LD_PRELOAD /system/lib64/liblogshim.so
  --setenv LD_LIBRARY_PATH "/apex/com.android.art/lib64:/apex/com.android.os.statsd/lib64:/apex/com.android.i18n/lib64:/system/lib64:/apex/sharedlibs/lib64"
)
# The log interposer is not in the product sysroot; copy it in (artroot.sh does
# the same). It is a host-built .so, not an ART artefact.
if [ -f "$LOGSHIM" ]; then
    cp -f "$LOGSHIM" "$S/system/lib64/liblogshim.so"
else
    # Without it dex2oat still works; log lines are just less useful.
    BWRAP=("${BWRAP[@]/--setenv LD_PRELOAD \/system\/lib64\/liblogshim.so/}")
fi

ARGS=(
  "--image=/data/img/x86_64/boot.art"
  "--base=0x70000000"
  "--oat-file=/data/img/x86_64/boot.oat"
  "--instruction-set=x86_64"
  "--image-format=$FMT"
)
IFS=':' read -r -a JARS <<<"$BOOTCP"
for j in "${JARS[@]}"; do
    ARGS+=("--dex-file=$j" "--dex-location=$j")
done

echo "B2: dex2oat64 --image-format=$FMT, ${#JARS[@]} boot jars -> $OUT/x86_64" >&2
exec bwrap "${BWRAP[@]}" -- /apex/com.android.art/bin/dex2oat64 "${ARGS[@]}"
