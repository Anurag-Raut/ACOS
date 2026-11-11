#!/usr/bin/env bash
# extract-sysroot.sh -- extract the pieces of a real Android 14 x86_64 system
# image that Google ART needs to *start* without Android: bionic linker+libs,
# the com.google.android.art APEX payload (dalvikvm64, libart*, javalib,
# boot images), com.android.runtime (bionic in an APEX), and the i18n/tzdata
# data the ICU natives read at start-up.
#
# Method: GPT -> super (sparse) -> lpunpack system -> debugfs (no mounts,
# no root, no sudo).  All output goes to .cache/art-exp/sysroot/ mirroring the
# on-device absolute paths so hard-coded /apex/... strings can be satisfied by
# a bind mount later.
set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../../.." && pwd)"
SDK="${ANDROID_HOME:-${ANDROID_SDK_ROOT:-/home/Qui/Android/Sdk}}"
IMG_DIR="$SDK/system-images/android-34/google_apis/x86_64"
OUT="$ROOT/.cache/art-exp/sysroot"
WORK="$ROOT/.cache/art-exp/work"
LOG="$ROOT/.cache/art-exp/logs"

log() { printf '[extract-sysroot] %s\n' "$*"; }
die() { printf '[extract-sysroot] error: %s\n' "$*" >&2; exit 1; }

mkdir -p "$OUT" "$WORK" "$LOG"
[ -f "$IMG_DIR/system.img" ] || die "no system image at $IMG_DIR"

# --- 1. GPT -> super -------------------------------------------------------
if [ ! -f "$WORK/super.img" ]; then
    read -r START SECS <<<"$(python3 - "$IMG_DIR/system.img" <<'PY'
import struct, sys
f = open(sys.argv[1], 'rb'); f.seek(512); h = f.read(512)
assert h[:8] == b'EFI PART', 'not a GPT disk'
plba, num, esz = struct.unpack_from('<Q', h, 72)[0], struct.unpack_from('<I', h, 80)[0], struct.unpack_from('<I', h, 84)[0]
f.seek(plba * 512)
for _ in range(num):
    e = f.read(esz)
    if len(e) < 128 or e[:16] == b'\0' * 16:
        continue
    if e[56:128].decode('utf-16le').rstrip('\0') == 'super':
        first, last = struct.unpack_from('<QQ', e, 32)
        print(first, last - first + 1); break
else:
    sys.exit('no super partition')
PY
)" || die "GPT parse failed"
    log "dd super.img (start=$START sectors=$SECS)"
    dd if="$IMG_DIR/system.img" of="$WORK/super.img" bs=512 skip="$START" \
        count="$SECS" conv=sparse status=none || die "dd failed"
fi

# --- 2. super -> system.img -----------------------------------------------
SYSIMG="$WORK/system.img"
if [ ! -f "$SYSIMG" ]; then
    log "lpunpack system"
    mkdir -p "$WORK/lp"
    lpunpack -p system "$WORK/super.img" "$WORK/lp" >"$LOG/lpunpack.log" 2>&1 \
        || die "lpunpack failed (see $LOG/lpunpack.log)"
    mv "$WORK/lp/system.img" "$SYSIMG"
fi
[ -f "$SYSIMG" ] || die "no system.img"

# --- 3. /system (non-APEX) files ------------------------------------------
CMDS="$WORK/debugfs.cmds"
: >"$CMDS"
want_dir() { printf 'rdump %s %s\n' "$1" "$2" >>"$CMDS"; }
mkdir -p "$OUT/system/bin" "$OUT/system/lib64" "$OUT/system/framework" \
         "$OUT/system/etc" "$OUT/system/apex"

# linker64 and the non-APEX bionic libs (present on x86_64 images)
for f in linker64 linker libc.so libm.so libdl.so liblog.so libc++.so \
         libc++_shared.so libnetd_client.so libnativehelper.so; do
    printf 'dump /system/bin/%s %s/system/bin/%s\n' "$f" "$OUT" "$f" >>"$CMDS"
    printf 'dump /system/lib64/%s %s/system/lib64/%s\n' "$f" "$OUT" "$f" >>"$CMDS"
done

# --- 4. APEXes of interest ------------------------------------------------
APEXES="com.google.android.art com.android.runtime com.android.i18n \
com.google.android.tzdata5 com.google.android.tzdata4 com.android.tzdata"

for apex in $APEXES; do
    apexf="$WORK/$apex.apex"
    [ -f "$apexf" ] || printf 'dump /system/apex/%s.apex %s\n' "$apex" "$apexf" >>"$CMDS"
done

log "debugfs pass 1 (system files + apex archives)"
debugfs -f "$CMDS" "$SYSIMG" >"$LOG/debugfs1.log" 2>&1

# Strip any raw APEX payloads (original_apex wrapper handling) and extract
for apex in $APEXES; do
    apexf="$WORK/$apex.apex"
    [ -s "$apexf" ] || { log "  ($apex: not in image)"; continue; }
    payload="$WORK/$apex.img"
    if [ ! -f "$payload" ]; then
        d="$WORK/x-$apex"; rm -rf "$d"; mkdir -p "$d"
        if unzip -l "$apexf" 2>/dev/null | grep -q original_apex; then
            unzip -o -q "$apexf" original_apex -d "$d" || true
            unzip -o -q "$d/original_apex" apex_payload.img -d "$d/p" || true
            mv "$d/p/apex_payload.img" "$payload" 2>/dev/null || true
        else
            unzip -o -q "$apexf" apex_payload.img -d "$d" || true
            mv "$d/apex_payload.img" "$payload" 2>/dev/null || true
        fi
    fi
    [ -f "$payload" ] || { log "  ($apex: no payload)"; continue; }
    dest="$OUT/system/apex/$apex"
    mkdir -p "$dest"
    log "extracting payload $apex"
    for d in bin lib64 lib javalib etc framework; do
        debugfs -R "rdump /$d $dest" "$payload" >>"$LOG/debugfs-$apex.log" 2>&1
    done
    # boot images live at the apex root or under /etc
    debugfs -R "ls -l /" "$payload" 2>/dev/null | awk '{print $NF}' \
        | grep -E '\.(art|oat|vdex|odex)$' | while read -r f; do
            debugfs -R "dump /$f $dest/$f" "$payload" >>"$LOG/debugfs-$apex.log" 2>&1
        done
done

# --- 5. non-APEX framework jars (BOOTCLASSPATH tail) ----------------------
log "dump /system/framework jars"
FRAMEWORK_LIST="$(debugfs -R 'ls -l /system/framework' "$SYSIMG" 2>/dev/null || true)"
: >"$CMDS"
printf '%s\n' "$FRAMEWORK_LIST" | awk '{print $NF}' | grep -E '\.jar$' | sort -u \
    | while read -r j; do printf 'dump /system/framework/%s %s/system/framework/%s\n' "$j" "$OUT" "$j" >>"$CMDS"; done
printf 'dump /system/build.prop %s/system/build.prop\n' "$OUT" >>"$CMDS"
printf 'dump /system/etc/preloaded-classes %s/system/etc/preloaded-classes\n' "$OUT" >>"$CMDS"
debugfs -f "$CMDS" "$SYSIMG" >>"$LOG/debugfs2.log" 2>&1

# --- 6. remove zero-byte dump failures, report ----------------------------
find "$OUT" -type f -size 0 -delete 2>/dev/null

{
  echo "# sysroot manifest (extracted $(date -Is) from $IMG_DIR)"
  echo "# path                                              bytes  sha256"
  ( cd "$OUT" && find . -type f -printf '%p %s\n' | sort ) | while read -r p s; do
      printf '%-52s %10s  %s\n' "$p" "$s" "$(sha256sum "$OUT/$p" | cut -c1-16)"
  done
} >"$OUT/MANIFEST.txt"

log "sysroot size: $(du -sh "$OUT" | cut -f1)"
log "key files:"
for f in system/bin/linker64 system/lib64/libc.so \
         system/apex/com.android.runtime/bin/linker64 \
         system/apex/com.google.android.art/bin/dalvikvm64 \
         system/apex/com.google.android.art/lib64/libart.so; do
    [ -f "$OUT/$f" ] && log "  OK   $f ($(stat -c%s "$OUT/$f") bytes)" || log "  MISS $f"
done
ls "$OUT/system/apex/com.google.android.art/javalib" 2>/dev/null | head -20

# READY is written by the caller after verification (see finish-sysroot.sh)
log "extraction done; MANIFEST.txt written"
