#!/usr/bin/env bash
# e2droot.sh -- artroot.sh + (a) extra LD_PRELOAD libraries and (b) extra
# read-only binds, for the E2D framework-substrate experiment.
# Copied from research/art-feasibility/a-dalvikvm/scripts/artroot.sh (E1A) so
# that the E1A scripts stay untouched; only the ENVV block differs.
#
#   E2D_PRELOAD=/data/lib/libbindershim.so scripts/e2droot.sh dalvikvm -cp ... Main binder
#
# original header follows.
#
# artroot.sh -- run a command inside a *mini-Android root* assembled with
# unprivileged user+mount namespaces (bubblewrap): the real /system, every
# extracted APEX mounted at /apex/<module> (module names from the APEX
# manifests, exactly like apexd), a `linkerconfig`-generated /linkerconfig, and
# a writable /data.  No root, no sudo, no init/zygote/system_server, no boot.
#
#   scripts/artroot.sh /system/bin/sh -c 'ls /apex'
#   scripts/artroot.sh dalvikvm -cp /data/dex/01_hello Main
#   scripts/artroot.sh /apex/com.android.art/bin/dex2oat64 --help
#
# Why the namespaces: ART hard-codes /system/... and /apex/com.android.art/...,
# and libnativeloader requires the linker to have created the `system` and
# per-APEX namespaces (from /linkerconfig/ld.config.txt).  Both are impossible
# without a mount namespace at those exact paths.
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
. "$HERE/../../a-dalvikvm/scripts/env.sh"

S="$ART_SYSROOT"
DATA="${ART_DATA:-$ACOS_ROOT/.cache/art-exp/data}"
LC="$DATA/linkerconfig"
LOGSHIM="$ACOS_ROOT/.cache/art-exp/shim/liblogshim.so"
mkdir -p "$DATA" "$LC" "$DATA/dalvik-cache/x86_64" \
         "$DATA/misc/apexdata/com.android.art/dalvik-cache/x86_64"

# The log interposer must be reachable inside the mini-root; /system/lib64 is
# the only writable spot in the (otherwise read-only) sysroot.
if [ ! -f "$S/system/lib64/liblogshim.so" ] || [ "$LOGSHIM" -nt "$S/system/lib64/liblogshim.so" ]; then
    cp -f "$LOGSHIM" "$S/system/lib64/liblogshim.so"
fi

# --- /apex/apex-info-list.xml (normally written by apexd at boot) ----------
if [ ! -f "$DATA/apex-info-list.xml" ]; then
    {
        echo '<?xml version="1.0" encoding="utf-8"?>'
        echo '<apex-info-list>'
        for d in "$S/system/apex"/*; do
            [ -d "$d" ] || continue
            m="$(basename "$d")"
            # the APEX *file* name differs from the module name only for the
            # Google-signed ART module
            f="com.${m#com.}.apex"
            [ -f "$S/system/apex/$m/../$m.apex" ] || true
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

# Host -> device path rewriting: $S/system/apex/<m>/x -> /apex/<m>/x, $S/x -> /x.
devpath() { printf '%s' "$1" | sed -e "s#$S/system/apex/#/apex/#g" -e "s#$S##g"; }

BWRAP_ARGS=(
  --tmpfs /
  --ro-bind "$S/system" /system
  --dir /apex
  --ro-bind "$DATA/apex-info-list.xml" /apex/apex-info-list.xml
)
# Every APEX at its on-device mount point.  The SDK-versioned shared-library
# APEX is mounted at /apex/sharedlibs (that is what the /apex/sharedlibs/...
# symlinks inside other APEXes point at).
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
    [ -n "${E2D_PRELOAD:-}" ] && ENVV+=(--setenv LD_PRELOAD "$E2D_PRELOAD")
else
    ENVV+=(--setenv LD_PRELOAD "/system/lib64/liblogshim.so${E2D_PRELOAD:+:$E2D_PRELOAD}")
fi
# Optional: host-side helper binaries (dex2oat64) need their own APEX lib dir
# on the search path; the device relies on linker namespaces instead.
[ -n "${ART_DEV_LD_LIBRARY_PATH:-}" ] && ENVV+=(--setenv LD_LIBRARY_PATH "$ART_DEV_LD_LIBRARY_PATH")

# One-time linker-namespace config, generated by the image's own `linkerconfig`
# from /system/etc/linker.config.pb plus the APEX manifests.
if [ ! -f "$LC/ld.config.txt" ] || [ "${ART_REGEN_LD_CONFIG:-}" = "1" ]; then
    bwrap "${BWRAP_ARGS[@]}" -- /apex/com.android.runtime/bin/linkerconfig \
        --target /linkerconfig >"$DATA/linkerconfig.log" 2>&1
    grep -c . "$LC/ld.config.txt" >/dev/null 2>&1 || {
        echo "artroot: linkerconfig failed" >&2; sed 's/^/  /' "$DATA/linkerconfig.log" >&2; }
fi

if [ "${1:-}" = "dalvikvm" ]; then
    shift
    exec bwrap "${BWRAP_ARGS[@]}" "${ENVV[@]}" -- \
        /system/apex/com.android.art/bin/dalvikvm64 "$@"
fi
exec bwrap "${BWRAP_ARGS[@]}" "${ENVV[@]}" -- "$@"
