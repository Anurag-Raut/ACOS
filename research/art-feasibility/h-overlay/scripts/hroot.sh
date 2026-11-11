#!/usr/bin/env bash
# hroot.sh -- e2droot.sh + the font set, provisioned *into* the E1A sysroot.
#
# E2D's mini-root has no /system/fonts and no /system/etc/fonts.xml, so
# Typeface/SystemFonts cannot build a font map and every text layout comes out
# empty. bionic/bwrap cannot create the mount point inside the read-only
# /system bind (measured: "bwrap: Can't create file /system/fonts: Read-only
# file system"), so we provision the sysroot instead: hard links from the
# extracted image font set (.cache/boot/android-34/fonts, same filesystem) --
# no copy, no space, and the E2D scripts stay untouched.
#
#   E2D_PRELOAD=/data/lib/libbindershim.so scripts/hroot.sh dalvikvm -cp .. Main all
set -uo pipefail
HOV="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ACOS_ROOT="$(cd "$HOV/../../../.." && pwd)"
. "$HOV/../../a-dalvikvm/scripts/env.sh"

FONTS="$ACOS_ROOT/.cache/boot/android-34/fonts"
S="$ART_SYSROOT"

if [ -d "$FONTS" ]; then
    if [ ! -f "$S/system/fonts/Roboto-Regular.ttf" ]; then
        mkdir -p "$S/system/fonts"
        cp -al "$FONTS"/. "$S/system/fonts"/ 2>/dev/null ||
            cp -a "$FONTS"/. "$S/system/fonts"/
        echo "hroot: provisioned $(ls "$S/system/fonts" | wc -l) fonts into $S/system/fonts" >&2
    fi
    if [ ! -f "$S/system/etc/fonts.xml" ] && [ -f "$FONTS/fonts.xml" ]; then
        cp -al "$FONTS/fonts.xml" "$S/system/etc/fonts.xml" 2>/dev/null ||
            cp -a "$FONTS/fonts.xml" "$S/system/etc/fonts.xml"
    fi
else
    echo "hroot: WARNING no extracted fonts at $FONTS -- text will not render" >&2
fi

exec "$HOV/../../d-framework/scripts/e2droot.sh" "$@"
