#!/usr/bin/env bash
# B2: run the probe under the same mini-Android root as build-uncompressed.sh,
# with `-Ximage` + optional `-Xnorelocate`, to test whether a *fixed* image base
# keeps the uncompressed image file-backed/shared instead of relocated-private.
# Stock dalvikvm64 only; nothing here touches product code.
set -euo pipefail
ROOT=/home/Qui/Projects/acos
S="$ROOT/.cache/art/sysroot"
WORK="$ROOT/.cache/b2/artroot"
IMG="$ROOT/.cache/b2/image"
FIX=1
DEV=""
while [ $# -gt 0 ]; do
    case "$1" in
        --image) IMG=$2; shift 2 ;;
        --relocate) FIX=0; shift ;;
        --hold-ms) DEV="--hold-ms $2"; shift 2 ;;
        *) echo "usage: $0 [--image DIR] [--relocate] [--hold-ms N]" >&2; exit 2 ;;
    esac
done
mkdir -p "$WORK/data/dalvik-cache/x86_64" "$WORK/data/task"
cp -f "$ROOT/.cache/b2/probe/classes.dex" "$WORK/data/task/classes.dex"
CP=$(sed -n 's/^export BOOTCLASSPATH //p' "$ROOT/.cache/art/data/system/environ/classpath")
BWRAP=(--tmpfs / --ro-bind "$S/system" /system --dir /apex
  --ro-bind "$WORK/apex-info-list.xml" /apex/apex-info-list.xml)
for d in "$S/system/apex"/*; do
  m=$(basename "$d")
  case "$m" in
    com.google.mainline.primary.libs) BWRAP+=(--ro-bind "$d" /apex/sharedlibs) ;;
    com.google.*) BWRAP+=(--ro-bind "$d" "/apex/com.android.${m#com.google.android.}") ;;
    *) BWRAP+=(--ro-bind "$d" "/apex/$m") ;;
  esac
done
BWRAP+=(--bind "$WORK/linkerconfig" /linkerconfig --bind "$WORK/data" /data
  --ro-bind "$IMG" /data/img --dev /dev --proc /proc --ro-bind /sys /sys
  --dir /vendor --dir /product --dir /system_ext --ro-bind /etc/passwd /etc/passwd
  --clearenv --setenv ANDROID_ROOT /system
  --setenv ANDROID_RUNTIME_ROOT /apex/com.android.runtime
  --setenv ANDROID_ART_ROOT /apex/com.android.art
  --setenv ANDROID_I18N_ROOT /apex/com.android.i18n
  --setenv ANDROID_TZDATA_ROOT /apex/com.google.android.tzdata5
  --setenv ANDROID_DATA /data --setenv BOOTCLASSPATH "$CP"
  --setenv DEX2OATBOOTCLASSPATH "$CP"
  --setenv LD_PRELOAD /system/lib64/liblogshim.so)
FLAGS=(-Ximage:/data/img/boot.art)
[ "$FIX" = 1 ] && FLAGS+=(-Xnorelocate)
exec bwrap "${BWRAP[@]}" -- /system/apex/com.android.art/bin/dalvikvm64 \
  "${FLAGS[@]}" -cp /data/task/classes.dex b2.HeapProbe --maps $DEV
