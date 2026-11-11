#!/usr/bin/env bash
#
# art-sysroot.sh -- reproducible, idempotent extraction of the ART sysroot the
# `acos-art` launcher needs, from the installed Android 14 (API 34) x86_64 SDK
# system image, plus an AOT boot image built by the image's own `dex2oat64`.
#
# Everything is unprivileged and offline: GPT parsing -> `lpunpack` of the
# `system` logical partition -> `debugfs` reads (no mounts, no root, no sudo).
# The SDK image is only ever read.
#
# Output layout (mirrors the device after apexd, which is what the bwrap sandbox
# binds):
#   <out>/system/{bin,etc,framework,lib,lib64}/...        real /system
#   <out>/system/apex/<module>/...                        APEX payloads, <module> == /apex/<module>
#   <out>/{product,system_ext,vendor}/fonts/...           the other partitions' font sets
#   <out>/{product,system_ext,vendor}/etc/fonts_customization.xml   (when present;
#                                                        `.no-fonts` marks a
#                                                        partition without /fonts)
#   <out>/FONTS.txt, <out>/APEX_MODULES.txt               module <TAB> count/file name
#   <out>/BOOTCLASSPATH                                   device paths, real boot order
#   <out>/bootimg/x86_64/boot.{art,oat,vdex}              AOT boot image (optional)
#   <out>/MANIFEST.txt, <out>/READY                       integrity + completion stamps
#
# Usage:
#   tools/art-sysroot.sh [--out DIR] [--force] [--no-boot-image] [--regen-classpath]
#
#   --out DIR            output directory (default .cache/art/sysroot)
#   --force              redo the extraction even if READY exists
#   --no-boot-image      skip the dex2oat64 boot image step
#   --image REL          system image, relative to $ANDROID_HOME
#                        (default system-images/android-34/google_apis/x86_64)
#   --keep-work          keep the ~5 GB intermediates (super.img, system.img)
#
# Idempotency: a run with a complete READY stamp and the derived files present
# only re-creates missing pieces (e.g. the boot image); --force wipes the output
# first. Intermediates live in <out>/.work and are removed on exit (also on
# failure) unless --keep-work is given.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SDK="${ANDROID_HOME:-${ANDROID_SDK_ROOT:-$HOME/Android/Sdk}}"
IMAGE_REL="${ANDROID_IMAGE_REL:-system-images/android-34/google_apis/x86_64}"
OUT="${ACOS_ART_SYSROOT:-$ROOT/.cache/art/sysroot}"
BOOT_IMAGE=1
FORCE=0
KEEP_WORK=0

log() { printf '[art-sysroot] %s\n' "$*"; }
die() { printf '[art-sysroot] error: %s\n' "$*" >&2; exit 1; }

while [ $# -gt 0 ]; do
    case "$1" in
        --out) OUT="$2"; shift 2 ;;
        --image) IMAGE_REL="$2"; shift 2 ;;
        --force) FORCE=1; shift ;;
        --no-boot-image) BOOT_IMAGE=0; shift ;;
        --keep-work) KEEP_WORK=1; shift ;;
        -h|--help) sed -n '2,40p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'; exit 0 ;;
        *) die "unknown option $1 (try --help)" ;;
    esac
done

IMG_DIR="$SDK/$IMAGE_REL"
WORK="$OUT/.work"
SYSIMG="$WORK/system.img"
[ -f "$IMG_DIR/system.img" ] || die "system image not found: $IMG_DIR/system.img"

need() {
    local t="$1"
    command -v "$t" >/dev/null 2>&1 || die "required tool not on PATH: $t"
}
for t in dd lpunpack debugfs unzip python3 sha256sum strings; do need "$t"; done

cleanup() {
    if [ "$KEEP_WORK" -ne 1 ]; then rm -rf "$WORK"; fi
}
trap cleanup EXIT

# ---------------------------------------------------------------------------
# 1. system.img (whole-disk GPT) -> super (sparse) -> system (ext4)
# ---------------------------------------------------------------------------
super_geometry() {
    python3 - "$IMG_DIR/system.img" <<'PY'
import struct, sys
with open(sys.argv[1], "rb") as f:
    f.seek(512)
    hdr = f.read(512)
    if hdr[:8] != b"EFI PART":
        sys.exit("not a GPT disk image")
    part_lba = struct.unpack_from("<Q", hdr, 72)[0]
    count = struct.unpack_from("<I", hdr, 80)[0]
    esize = struct.unpack_from("<I", hdr, 84)[0]
    f.seek(part_lba * 512)
    for _ in range(count):
        e = f.read(esize)
        if len(e) < 128 or e[:16] == b"\0" * 16:
            continue
        if e[56:128].decode("utf-16le").rstrip("\0") == "super":
            first, last = struct.unpack_from("<QQ", e, 32)
            print(first, last - first + 1)
            break
    else:
        sys.exit("no `super` partition in the GPT")
PY
}

if [ "$FORCE" -eq 1 ]; then
    log "--force: removing $OUT"
    rm -rf "$OUT"
fi
mkdir -p "$OUT" "$WORK"

extra_missing() {
    local p
    for p in product system_ext vendor; do
        [ -d "$OUT/$p/fonts" ] || [ -f "$OUT/$p/.no-fonts" ] || return 0
    done
    return 1
}

if [ -f "$SYSIMG" ] && extra_missing; then
    log "system.img present but the font partitions are not provisioned yet: re-extracting"
    rm -f "$SYSIMG"
fi

if [ ! -f "$SYSIMG" ]; then
    read -r SUPER_START SUPER_SECTORS <<<"$(super_geometry)" || die "GPT parse failed"
    log "dd super.img (start=$SUPER_START sectors=$SUPER_SECTORS)"
    dd if="$IMG_DIR/system.img" of="$WORK/super.img" bs=512 skip="$SUPER_START" \
        count="$SUPER_SECTORS" conv=sparse status=none || die "dd failed"
    log "lpunpack system"
    mkdir -p "$WORK/lp"
    # The other read-only partitions are only unpacked when their font set has
    # not been provisioned yet (see step 2b); on a fully provisioned sysroot
    # this call is skipped and the run stays cheap.
    extra_parts=()
    for p in product system_ext vendor; do
        [ -d "$OUT/$p/fonts" ] || [ -f "$OUT/$p/.no-fonts" ] || extra_parts+=("-p" "$p")
    done
    lpunpack -p system "${extra_parts[@]}" "$WORK/super.img" "$WORK/lp" >"$WORK/lpunpack.log" 2>&1 \
        || die "lpunpack failed (see $WORK/lpunpack.log)"
    mv "$WORK/lp/system.img" "$SYSIMG"
    rm -f "$WORK/super.img"
else
    log "reusing $SYSIMG ($(du -h "$SYSIMG" | cut -f1))"
fi
[ -f "$SYSIMG" ] || die "no system.img after extraction"

# ---------------------------------------------------------------------------
# 2. /system (non-APEX content) via debugfs, no mount
# ---------------------------------------------------------------------------
export_system() {
    local d
    mkdir -p "$OUT/system"
    for d in bin etc framework lib lib64; do
        # debugfs rdump needs the destination *parent* to exist and creates
        # <dest>/<basename(source)>; pre-creating <out>/system/<d> makes it fail
        # with "File exists while making directory".
        rm -rf "$OUT/system/$d"
        debugfs -R "rdump /system/$d $OUT/system" "$SYSIMG" >"$WORK/debugfs-$d.log" 2>&1 || true
    done
    # The META / lost+found dirs debugfs can create are not device paths.
    rm -rf "$OUT/system/lost+found" "$OUT/system/.#*" 2>/dev/null || true
}

if [ ! -d "$OUT/system/framework" ] || [ -z "$(ls -A "$OUT/system/framework" 2>/dev/null)" ]; then
    log "extracting /system (bin, etc, framework, lib, lib64)"
    export_system
fi
[ -f "$OUT/system/framework/framework.jar" ] || die "no framework.jar: /system extraction failed"

# ---------------------------------------------------------------------------
# 2b. the other read-only partitions' font sets (/product, /system_ext,
#     /vendor): `fonts/` + `etc/fonts_customization.xml`, nothing else.
#
#     On a device the Google families the image's static RROs point
#     `config_*FontFamily` at (`google-sans`, `google-sans-text`, ...) live in
#     `/product/fonts`, and `SystemFonts.getSystemPreinstalledFontConfig()`
#     reads `/product/etc/fonts_customization.xml` (verified in the image's
#     `SystemFonts` bytecode: the four constants are `/system/etc/fonts.xml`,
#     `/system/fonts/`, `/product/etc/fonts_customization.xml`,
#     `/product/fonts/`). Neither is under `/system`, so a sysroot that carries
#     only `/system/fonts` cannot resolve those families.
#
#     Extracted with the same read-only `debugfs` path as `/system` -- no
#     mounts, no root, no sudo -- and kept next to the partition they come from
#     (`<out>/product/...`), which is what `ensure_system_fonts` links into the
#     sandbox's `/system` bind. A partition without `/fonts` gets a `.no-fonts`
#     marker so a re-run does not unpack it again (idempotency).
# ---------------------------------------------------------------------------
export_partition_fonts() {
    local part img dest count n
    : >"$WORK/FONTS.txt"
    for part in product system_ext vendor; do
        img="$WORK/lp/$part.img"
        dest="$OUT/$part"
        count=0
        if [ -f "$img" ]; then
            # `ls -l` always lists `.` and `..`.
            count="$(debugfs -R 'ls -l /fonts' "$img" 2>/dev/null \
                | grep -cE '^ *[0-9]+ ' || true)"
            count=$((count > 2 ? count - 2 : 0))
        fi
        if [ "$count" -eq 0 ]; then
            log "  $part: no /fonts (image ships none)"
            mkdir -p "$dest"
            : >"$dest/.no-fonts"
            printf '%s\t%s\n' "$part" "0" >>"$WORK/FONTS.txt"
            rm -f "$img"
            continue
        fi
        rm -rf "$dest/fonts"
        mkdir -p "$dest"
        debugfs -R "rdump /fonts $dest" "$img" >"$WORK/debugfs-$part-fonts.log" 2>&1 || true
        mkdir -p "$dest/etc"
        debugfs -R "dump /etc/fonts_customization.xml $dest/etc/fonts_customization.xml" \
            "$img" >>"$WORK/debugfs-$part-fonts.log" 2>&1 || true
        # `dump` leaves a 0-byte file behind when the image has no such file.
        [ -s "$dest/etc/fonts_customization.xml" ] || rm -f "$dest/etc/fonts_customization.xml"
        rm -rf "$dest/lost+found" "$dest/.#"* 2>/dev/null || true
        n="$(find "$dest/fonts" -type f 2>/dev/null | wc -l)"
        printf '%s\t%s\n' "$part" "$n" >>"$WORK/FONTS.txt"
        if [ -f "$dest/etc/fonts_customization.xml" ]; then
            log "  $part: $n font(s), fonts_customization.xml"
        else
            log "  $part: $n font(s)"
        fi
        rm -f "$img"
    done
    sort -u "$WORK/FONTS.txt" >"$OUT/FONTS.txt"
}

if extra_missing; then
    log "extracting the font sets of /product /system_ext /vendor"
    export_partition_fonts
fi

# ---------------------------------------------------------------------------
# 3. every APEX payload -> <out>/system/apex/<module>/
#    (module name from apex_manifest.pb, i.e. the on-device mount point)
# ---------------------------------------------------------------------------
export_apexes() {
    local listing apexfile base apex payload module dest d
    listing="$(debugfs -R 'ls -l /system/apex' "$SYSIMG" 2>/dev/null \
        | awk '{print $NF}' | grep '\.apex$' | sort -u)"
    [ -n "$listing" ] || die "could not list /system/apex"
    : >"$WORK/APEX_MODULES.txt"
    for apexfile in $listing; do
        base="${apexfile%.apex}"
        apex="$WORK/$apexfile"
        [ -f "$apex" ] || debugfs -R "dump /system/apex/$apexfile $apex" "$SYSIMG" 2>/dev/null || true
        [ -s "$apex" ] || { log "  $apexfile: missing from image"; continue; }

        payload="$WORK/p-$base.img"
        if [ ! -f "$payload" ]; then
            rm -rf "$WORK/px-$base"; mkdir -p "$WORK/px-$base"
            if unzip -o -q "$apex" apex_payload.img -d "$WORK/px-$base" 2>/dev/null; then
                mv "$WORK/px-$base/apex_payload.img" "$payload"
            elif unzip -o -q "$apex" original_apex -d "$WORK/px-$base" 2>/dev/null; then
                unzip -o -q "$WORK/px-$base/original_apex" apex_payload.img -d "$WORK/px-$base/p" 2>/dev/null && \
                    mv "$WORK/px-$base/p/apex_payload.img" "$payload"
            fi
        fi
        [ -f "$payload" ] || { log "  $apexfile: no apex_payload.img"; continue; }

        # module name = first string in apex_manifest.pb that looks like a
        # package name; falls back to the apex file name.
        # NOTE: `set -e` + command substitution + grep(1) is a live grenade here
        # (a manifest without a matching string used to abort the whole run);
        # every step is guarded and falls back to the .apex file name.
        module=""
        if debugfs -R "dump /apex_manifest.pb $WORK/m.pb" "$payload" >/dev/null 2>&1; then
            module="$(strings -n 6 "$WORK/m.pb" 2>/dev/null \
                | grep -E '^[a-z][a-z0-9_]*(\.[a-z0-9_]+){2,}$' | head -1 || true)"
        fi
        [ -n "$module" ] || module="$base"

        dest="$OUT/system/apex/$module"
        if [ ! -d "$dest" ] || [ -z "$(ls -A "$dest" 2>/dev/null)" ]; then
            mkdir -p "$dest"
            for d in bin lib lib64 javalib etc framework; do
                debugfs -R "rdump /$d $dest" "$payload" >>"$WORK/rdump-$base.log" 2>&1 || true
            done
            debugfs -R "dump /apex_manifest.pb $dest/apex_manifest.pb" "$payload" >>"$WORK/rdump-$base.log" 2>&1 || true
        fi
        find "$dest" -type f -size 0 -delete 2>/dev/null || true
        printf '%s\t%s\n' "$module" "$apexfile" >>"$WORK/APEX_MODULES.txt"
        log "  $module  jars=$(ls "$dest/javalib" 2>/dev/null | wc -l)  libs=$(ls "$dest/lib64" 2>/dev/null | wc -l)"
    done
    sort -u "$WORK/APEX_MODULES.txt" >"$OUT/APEX_MODULES.txt"
}

if [ ! -s "$OUT/APEX_MODULES.txt" ] || [ -z "$(ls -A "$OUT/system/apex" 2>/dev/null)" ]; then
    log "extracting APEX payloads from $(basename "$IMG_DIR")"
    export_apexes
fi
nmodules=$(wc -l <"$OUT/APEX_MODULES.txt")
[ "$nmodules" -ge 30 ] || die "only $nmodules APEX modules extracted (expected ~34)"

# ---------------------------------------------------------------------------
# 4. device BOOTCLASSPATH: merge the per-APEX and /system classpath fragments
#    in the real order, then rewrite to device paths and drop absent jars.
# ---------------------------------------------------------------------------
derive_bootclasspath() {
    local cpsrc="$WORK/classpaths"
    mkdir -p "$cpsrc"
    debugfs -R "dump /system/etc/classpaths/bootclasspath.pb $cpsrc/system_bootclasspath.pb" \
        "$SYSIMG" >/dev/null 2>&1 || die "no /system/etc/classpaths/bootclasspath.pb"
    local apexfile base payload
    while IFS=$'\t' read -r module apexfile; do
        payload="$WORK/p-${apexfile%.apex}.img"
        [ -f "$payload" ] || continue
        debugfs -R "dump /etc/classpaths/bootclasspath.pb $cpsrc/${module}_bootclasspath.pb" \
            "$payload" >/dev/null 2>&1 || true
    done <"$OUT/APEX_MODULES.txt"

    python3 - "$cpsrc" "$OUT" <<'PY'
import os, re, sys

cpsrc, out = sys.argv[1], sys.argv[2]
apex_root = os.path.join(out, "system", "apex")
framework = os.path.join(out, "system", "framework")

# jar base name -> device path, from what actually got extracted
by_jar = {}
for module in sorted(os.listdir(apex_root)):
    javalib = os.path.join(apex_root, module, "javalib")
    mount = "/apex/sharedlibs" if module == "com.google.mainline.primary.libs" else "/apex/" + module
    if os.path.isdir(javalib):
        for jar in sorted(os.listdir(javalib)):
            if jar.endswith(".jar"):
                by_jar.setdefault(jar, f"{mount}/javalib/{jar}")
if os.path.isdir(framework):
    for jar in sorted(os.listdir(framework)):
        if jar.endswith(".jar"):
            by_jar.setdefault(jar, "/system/framework/" + jar)

# The ART APEX fragment first (core-oj/core-libart/...), then conscrypt, then
# /system/etc/classpaths (framework.jar, ext.jar, ...), then the rest — this is
# the order the device gets from derive_classpath.
preferred = []
for name in ("com.android.art", "com.google.android.art"):
    p = os.path.join(cpsrc, name + "_bootclasspath.pb")
    if os.path.exists(p):
        preferred.append(p)
        break
for name in ("com.android.conscrypt", "com.google.android.conscrypt"):
    p = os.path.join(cpsrc, name + "_bootclasspath.pb")
    if os.path.exists(p):
        preferred.append(p)
        break
preferred.append(os.path.join(cpsrc, "system_bootclasspath.pb"))
rest = sorted(
    os.path.join(cpsrc, f)
    for f in os.listdir(cpsrc)
    if f.endswith("_bootclasspath.pb") and os.path.join(cpsrc, f) not in preferred
)

order, seen, missing = [], set(), []
for src in preferred + rest:
    if not os.path.exists(src):
        continue
    data = open(src, "rb").read()
    for m in re.finditer(rb"(/(?:apex|system)/[^\x00]*?\.jar)", data):
        jar = os.path.basename(m.group(1).decode("ascii", "replace"))
        if jar in seen:
            continue
        seen.add(jar)
        if jar in by_jar:
            order.append(by_jar[jar])
        else:
            missing.append(jar)

with open(os.path.join(out, "BOOTCLASSPATH"), "w") as f:
    f.write("\n".join(order) + "\n")
with open(os.path.join(out, "BOOTCLASSPATH.missing"), "w") as f:
    f.write("\n".join(missing) + ("\n" if missing else ""))
print(f"{len(order)} jars, {len(missing)} named-but-absent")
PY
}

if [ ! -s "$OUT/BOOTCLASSPATH" ]; then
    log "deriving BOOTCLASSPATH from the image's classpath fragments"
    derive_bootclasspath
fi
[ -s "$OUT/BOOTCLASSPATH" ] || die "BOOTCLASSPATH derivation failed"
log "BOOTCLASSPATH: $(wc -l <"$OUT/BOOTCLASSPATH") jars (missing: $(grep -c . "$OUT/BOOTCLASSPATH.missing" 2>/dev/null || echo 0))"

# ---------------------------------------------------------------------------
# 5. AOT boot image, built by the image's own dex2oat64 inside the sandbox
# ---------------------------------------------------------------------------
build_boot_image() {
    local bin="$ROOT/target/debug/acos-art"
    if [ ! -x "$bin" ]; then
        log "building the acos-art launcher"
        ( cd "$ROOT" && cargo build -q -p acos-art ) || die "cargo build -p acos-art failed"
    fi
    log "building the AOT boot image with dex2oat64 (this takes a few seconds)"
    ACOS_ART_SYSROOT="$OUT" ACOS_ART_DATA="$ROOT/.cache/art/data" \
        "$bin" build-image --out "$OUT/bootimg" --force 2>&1 | sed 's/^/    /'
    [ -f "$OUT/bootimg/x86_64/boot.art" ] || die "boot image build produced no boot.art"
}

if [ "$BOOT_IMAGE" -eq 1 ]; then
    if [ -f "$OUT/bootimg/x86_64/boot.art" ]; then
        log "boot image present: $OUT/bootimg/x86_64/boot.art"
    else
        build_boot_image
    fi
fi

# ---------------------------------------------------------------------------
# 6. manifest + stamps
# ---------------------------------------------------------------------------
log "writing MANIFEST.txt"
{
    echo "# ART sysroot extracted from $IMAGE_REL"
    echo "# $(date -Is)  acos-art-sysroot v1"
    echo "# path                                                        bytes  sha256[:16]"
    ( cd "$OUT" && find . -path ./.work -prune -o -type f -print0 \
        | sort -z | xargs -0 -r stat -c '%n %s' ) \
    | while read -r p s; do
        printf '%-60s %10s  %s\n' "$p" "$s" "$(sha256sum "$OUT/$p" | cut -c1-16)"
    done
} >"$OUT/MANIFEST.txt"

{
    echo "android_api=34"
    echo "abi=x86_64"
    echo "source_image=$IMAGE_REL"
    echo "apex_modules=$nmodules"
    echo "bootclasspath_jars=$(wc -l <"$OUT/BOOTCLASSPATH")"
    echo "boot_image=$([ -f "$OUT/bootimg/x86_64/boot.art" ] && echo yes || echo no)"
    echo "extracted_at=$(date -Is)"
} >"$OUT/READY"

log "done: $OUT ($(du -sh --exclude=.work "$OUT" | cut -f1))"
log "next: cargo run -p acos-art -- run --sysroot $OUT --dex <classes.dex> --class Main"
