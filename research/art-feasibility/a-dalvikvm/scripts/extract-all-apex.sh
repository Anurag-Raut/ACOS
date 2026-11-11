#!/usr/bin/env bash
# extract-all-apex.sh -- extract the *payload* of every APEX in the Android 14
# x86_64 system image into .cache/art-exp/sysroot/system/apex/<module>/ ,
# using the APEX manifest's module name for the directory (which is also the
# on-device mount point /apex/<module>).
#
# Needed because the real BOOTCLASSPATH spans ~15 APEXes (core-icu4j from the
# i18n APEX, conscrypt, framework-connectivity, ...) and ART's JNI natives come
# from APEX /lib64 dirs.
set -uo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../../.." && pwd)"
SYSROOT="$ROOT/.cache/art-exp/sysroot"
WORK="$ROOT/.cache/art-exp/work"
LOG="$ROOT/.cache/art-exp/logs"
SYSIMG="$WORK/system.img"
mkdir -p "$LOG"

log() { printf '[extract-all-apex] %s\n' "$*"; }
[ -f "$SYSIMG" ] || { echo "run extract-sysroot.sh first ($SYSIMG missing)" >&2; exit 1; }

# module name lives in the APEX manifest; the apex *file* name is the Google
# module name (com.google.android.art) while the mount dir is the module name
# (com.android.art).
listing="$(debugfs -R 'ls -l /system/apex' "$SYSIMG" 2>/dev/null | awk '{print $NF}' | grep '\.apex$')"
: >"$SYSROOT/APEX_MODULES.txt"

for apexfile in $listing; do
    base="${apexfile%.apex}"
    apex="$WORK/$base.apex"
    [ -f "$apex" ] || debugfs -R "dump /system/apex/$apexfile $apex" "$SYSIMG" 2>/dev/null

    payload="$WORK/p-$base.img"
    if [ ! -f "$payload" ]; then
        d="$WORK/px-$base"; rm -rf "$d"; mkdir -p "$d"
        if unzip -o -q "$apex" apex_payload.img -d "$d" 2>/dev/null && [ -f "$d/apex_payload.img" ]; then
            mv "$d/apex_payload.img" "$payload"
        elif unzip -o -q "$apex" original_apex -d "$d" 2>/dev/null; then
            unzip -o -q "$d/original_apex" apex_payload.img -d "$d/p" 2>/dev/null \
                && mv "$d/p/apex_payload.img" "$payload"
        fi
    fi
    [ -f "$payload" ] || { log "  $base: no payload"; continue; }

    module="$(debugfs -R "dump /apex_manifest.pb $WORK/m.pb" "$payload" >/dev/null 2>&1; \
              strings -n 6 "$WORK/m.pb" 2>/dev/null | head -1)"
    case "$module" in
        *.*.*) ;;
        *) module="$base" ;;
    esac
    dest="$SYSROOT/system/apex/$module"

    if [ ! -d "$dest/javalib" ] && [ ! -d "$dest/lib64" ]; then
        mkdir -p "$dest"
        for d in bin lib64 javalib etc framework; do
            debugfs -R "rdump /$d $dest" "$payload" >>"$LOG/rdump-$base.log" 2>&1
        done
        debugfs -R "dump /apex_manifest.pb $dest/apex_manifest.pb" "$payload" >>"$LOG/rdump-$base.log" 2>&1
    fi
    rm -f "$dest"/**/0 2>/dev/null
    njars=$(ls "$dest/javalib" 2>/dev/null | wc -l)
    log "  $module  jars=$njars  libs=$(ls "$dest/lib64" 2>/dev/null | wc -l)"
    printf '%s\t%s\n' "$module" "$base" >>"$SYSROOT/APEX_MODULES.txt"
done
find "$SYSROOT" -type f -size 0 -delete 2>/dev/null
log "done: $(wc -l <"$SYSROOT/APEX_MODULES.txt") APEXes, $(du -sh "$SYSROOT" | cut -f1)"
