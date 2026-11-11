#!/usr/bin/env bash
# env.sh -- environment describing the extracted Android 14 sysroot for the
# real-ART feasibility experiment.  Source it:
#
#   . research/art-feasibility/a-dalvikvm/scripts/env.sh
#
# Layout produced by scripts/extract-sysroot.sh + scripts/extract-all-apex.sh:
#   .cache/art-exp/sysroot/system/...                     (real /system)
#   .cache/art-exp/sysroot/system/apex/<module>/...       (real APEX payloads,
#                                                          <module> == /apex/<module>)
set -u
: "${ACOS_ROOT:=$(cd "$(dirname "${BASH_SOURCE[0]}")/../../../.." && pwd)}"
export ACOS_ROOT
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

export ART_SYSROOT="$ACOS_ROOT/.cache/art-exp/sysroot"
export ANDROID_ROOT="$ART_SYSROOT/system"
export ANDROID_RUNTIME_ROOT="$ANDROID_ROOT/apex/com.android.runtime"
export ANDROID_ART_ROOT="$ANDROID_ROOT/apex/com.android.art"
export ANDROID_I18N_ROOT="$ANDROID_ROOT/apex/com.android.i18n"
export ANDROID_TZDATA_ROOT="$ANDROID_ROOT/apex/com.google.android.tzdata5"
export ANDROID_DATA="${ART_DATA:-$ACOS_ROOT/.cache/art-exp/data}"

# The real device BOOTCLASSPATH order (derived from the image's
# /system/etc/classpaths + per-APEX classpath fragments; see
# .cache/boot/android-34/BOOTCLASSPATH and bootclasspath.txt here).
_bc=""
while read -r jar; do
    [ -n "$jar" ] || continue
    hit=""
    for d in "$ANDROID_ROOT"/apex/*/javalib; do
        if [ -f "$d/$jar" ]; then hit="$d/$jar"; break; fi
    done
    [ -n "$hit" ] || hit="$ANDROID_ROOT/framework/$jar"
    [ -f "$hit" ] && _bc="$_bc$hit:"
done <"$HERE/../bootclasspath.txt"
export BOOTCLASSPATH="${_bc%:}"
export DEX2OATBOOTCLASSPATH="$BOOTCLASSPATH"

export LINKER64="$ANDROID_RUNTIME_ROOT/bin/linker64"
export DALVIKVM="$ANDROID_ART_ROOT/bin/dalvikvm64"
export DEX2OAT="$ANDROID_ART_ROOT/bin/dex2oat64"

# for the host-side (non-namespace) launch path in artrun.sh
_lp="$ANDROID_RUNTIME_ROOT/lib64/bionic:$ANDROID_RUNTIME_ROOT/lib64"
for d in "$ANDROID_ROOT"/apex/*/lib64 "$ANDROID_ROOT/lib64"; do _lp="$_lp:$d"; done
export ART_LD_LIBRARY_PATH="$_lp"
