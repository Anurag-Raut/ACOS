#!/usr/bin/env bash
# r2root.sh -- artroot.sh (E1A) with an isolated ART_DATA root and an extra
# read-only view of the R2 fixtures, for the "an APK's own libs on real ART"
# experiment.
#
# Copied from research/art-feasibility/d-framework/scripts/e2droot.sh so that
# the E2D and E1A scripts stay untouched; the differences are:
#   * ART_DATA defaults to .cache/art-exp-r2/data (so /data/app/... can hold the
#     extracted APKs + lib/x86_64 without disturbing other agents' experiments),
#   * the scratch root is bind-mounted at /data exactly as before.
#
#   scripts/r2root.sh dalvikvm -cp /data/dex/r2/classes.dex dev.acos.r2.R2Main hello APK LIBDIR
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
. "$HERE/../../a-dalvikvm/scripts/env.sh"

export ART_DATA="${ART_DATA:-$ACOS_ROOT/.cache/art-exp-r2/data}"
S="$ART_SYSROOT"
DATA="$ART_DATA"
LC="$DATA/linkerconfig"
LOGSHIM="$ACOS_ROOT/.cache/art-exp/shim/liblogshim.so"
mkdir -p "$DATA" "$LC" "$DATA/dalvik-cache/x86_64" \
         "$DATA/misc/apexdata/com.android.art/dalvik-cache/x86_64"

if [ ! -f "$S/system/lib64/liblogshim.so" ] || [ "$LOGSHIM" -nt "$S/system/lib64/liblogshim.so" ]; then
    cp -f "$LOGSHIM" "$S/system/lib64/liblogshim.so"
fi

if [ ! -f "$DATA/apex-info-list.xml" ]; then
    {
        echo '<?xml version="1.0" encoding="utf-8"?>'
        echo '<apex-info-list>'
        for d in "$S/system/apex"/*; do
            [ -d "$d" ] || continue
            m="$(basename "$d")"
            case "$m" in
                com.android.art) f=com.google.android.art.apex ;;
                *os.statsd) f=com.google.android.os.statsd.apex ;;
                com.google.android.tzdata5) f=com.google.android.tzdata5.apex ;;
                *) f="$m.apex" ;;
            esac
            printf '  <apex-info moduleName="%s" modulePath="/system/apex/%s" preinstalledModulePath="/system/apex/%s" versionCode="340000000" versionName="1" isFactory="true" isActive="true" lastUpdateMillis="0" provideSharedApexLibs="%s" />\n' \
                "$m" "$f" "$f" "$([ "$m" = com.google.mainline.primary.libs ] && echo true || echo false)"
        done
        echo '</apex-info-list>'
    } >"$DATA/apex-info-list.xml"
fi

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
  --dir /vendor
  --dir /product
  --dir /system_ext
  --ro-bind /etc/passwd /etc/passwd
)

ENVV=(
  --setenv ANDROID_ROOT /system
  --setenv ANDROID_RUNTIME_ROOT /apex/com.android.runtime
  --setenv ANDROID_ART_ROOT /apex/com.android.art
  --setenv ANDROID_I18N_ROOT /apex/com.android.i18n
  --setenv ANDROID_TZDATA_ROOT /apex/com.google.android.tzdata5
  --setenv ANDROID_DATA /data
  --setenv BOOTCLASSPATH "$(devpath "$BOOTCLASSPATH")"
  --setenv DEX2OATBOOTCLASSPATH "$(devpath "$DEX2OATBOOTCLASSPATH")"
)
if [ -n "${ART_NO_LOGSHIM:-}" ]; then
    [ -n "${R2_PRELOAD:-}" ] && ENVV+=(--setenv LD_PRELOAD "$R2_PRELOAD")
else
    ENVV+=(--setenv LD_PRELOAD "/system/lib64/liblogshim.so${R2_PRELOAD:+:$R2_PRELOAD}")
fi
[ -n "${ART_DEV_LD_LIBRARY_PATH:-}" ] && ENVV+=(--setenv LD_LIBRARY_PATH "$ART_DEV_LD_LIBRARY_PATH")
# Keep the host environment out of the app namespace (E1C §1: the classloader
# namespace's library path is inherited from LD_LIBRARY_PATH otherwise).
[ -n "${R2_APP_LD_LIBRARY_PATH:-}" ] && ENVV+=(--setenv LD_LIBRARY_PATH "$R2_APP_LD_LIBRARY_PATH")

if [ ! -f "$LC/ld.config.txt" ] || [ "${ART_REGEN_LD_CONFIG:-}" = "1" ]; then
    bwrap "${BWRAP_ARGS[@]}" -- /apex/com.android.runtime/bin/linkerconfig \
        --target /linkerconfig >"$DATA/linkerconfig.log" 2>&1
    grep -c . "$LC/ld.config.txt" >/dev/null 2>&1 || {
        echo "r2root: linkerconfig failed" >&2; sed 's/^/  /' "$DATA/linkerconfig.log" >&2; }
fi

if [ "${1:-}" = "dalvikvm" ]; then
    shift
    exec bwrap "${BWRAP_ARGS[@]}" "${ENVV[@]}" -- \
        /system/apex/com.android.art/bin/dalvikvm64 "$@"
fi
exec bwrap "${BWRAP_ARGS[@]}" "${ENVV[@]}" -- "$@"
