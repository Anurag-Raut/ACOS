#!/usr/bin/env bash
#
# extract-bootclasspath.sh — extract the Android 14 (API 34) boot classpath
# jars (and related system files) from the installed SDK system image into
# .cache/boot/android-34/ using ONLY unprivileged, already-installed tools.
#
# Method (see research/framework-dex/REPORT.md):
#   1. system.img is a whole-disk GPT image. Parse the GPT, dd the `super`
#      dynamic-partition extent out (sparse), lpunpack the `system` logical
#      partition out of it.
#   2. debugfs reads files out of the ext4 system image without mounting.
#   3. APEX files under /system/apex are ZIPs (some wrapped in an
#      `original_apex`); their ext4 `apex_payload.img` is read with debugfs.
#
# Output layout (under .cache/boot/android-34/):
#   jars/<name>.jar                     real system/APEX jars
#   dex/<jar-name>/classes*.dex         unzipped dex from each jar
#   BOOTCLASSPATH                       one jar name per line, real boot order
#   build.prop                          /system/build.prop from the image
#   fonts/                              /system/fonts/*.{ttf,otf,ttc} + fonts.xml
#   framework-res.apk                   /system/framework/framework-res.apk
#   manifest.json                       size + sha256 of every output file
#
# Reproducible + idempotent: if a complete manifest already exists the script
# skips extraction (use --force to redo). Large intermediates (super.img, the
# ext4 partition images, APEX payloads) live in a work dir under .cache and are
# deleted at the end, including on failure, so peak disk stays ~1-2 GB.
#
# Every tool used is unprivileged; no sudo, no mounts, no network.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SDK="${ANDROID_HOME:-${ANDROID_SDK_ROOT:-/home/Qui/Android/Sdk}}"
IMAGE_REL="${ANDROID_IMAGE_REL:-system-images/android-34/google_apis/x86_64}"
IMG_DIR="$SDK/$IMAGE_REL"
CACHE="${ACOS_CACHE:-$ROOT/.cache}"
OUT="$CACHE/boot/android-34"
WORK="$OUT/.work"
FORCE=0
[ "${1:-}" = "--force" ] && FORCE=1

log()  { printf '[extract-bootclasspath] %s\n' "$*"; }
die()  { printf '[extract-bootclasspath] error: %s\n' "$*" >&2; exit 1; }

# --- tool checks (fail loudly) ----------------------------------------------
need() {
    local t="$1" p
    p="$(command -v "$t" 2>/dev/null || true)"
    [ -n "$p" ] || die "required tool not found on PATH: $t"
    printf '%s' "$p"
}
DD="$(need dd)"
LPUNPACK="$(need lpunpack)"
DEBUGFS="$(need debugfs)"
UNZIP="$(need unzip)"
PYTHON="$(need python3)"
SHA256="$(need sha256sum)"
[ -f "$IMG_DIR/system.img" ] || die "system image not found: $IMG_DIR/system.img (set ANDROID_HOME/ANDROID_IMAGE_REL)"

# --- idempotency fast path ---------------------------------------------------
if [ "$FORCE" -ne 1 ] && [ -f "$OUT/manifest.json" ] && [ -f "$OUT/BOOTCLASSPATH" ]; then
    log "already extracted: $OUT (use --force to redo)"
    log "BOOTCLASSPATH:"
    sed 's/^/    /' "$OUT/BOOTCLASSPATH"
    exit 0
fi

# --- clean work dir, always, even on failure --------------------------------
cleanup() {
    rm -rf "$WORK"
}
trap cleanup EXIT
rm -rf "$OUT"
mkdir -p "$OUT/jars" "$OUT/dex" "$OUT/fonts" "$WORK"

debugfs_dump() { # <image> <abs-path-in-image> <dest>
    local img="$1" path="$2" dest="$3"
    mkdir -p "$(dirname "$dest")"
    "$DEBUGFS" -R "dump $path $dest" "$img" >/dev/null 2>&1 \
        || die "debugfs failed to dump $path from $img"
    [ -f "$dest" ] || die "debugfs produced no file for $path"
}

debugfs_dump_optional() { # <image> <abs-path-in-image> <dest>
    local img="$1" path="$2" dest="$3"
    if printf '%s\n' "$("$DEBUGFS" -R "stat $path" "$img" 2>/dev/null)" | grep -q '^Inode:'; then
        debugfs_dump "$img" "$path" "$dest"
        return 0
    fi
    return 1
}

# --- 1. GPT -> super -> system.img ------------------------------------------
log "reading GPT from $IMG_DIR/system.img"
SUPER_INFO="$("$PYTHON" - "$IMG_DIR/system.img" <<'PYEOF'
import struct, sys
img = sys.argv[1]
with open(img, "rb") as f:
    f.seek(512)
    hdr = f.read(512)
    if hdr[:8] != b"EFI PART":
        sys.exit("not a GPT disk image")
    part_lba = struct.unpack_from("<Q", hdr, 72)[0]
    num = struct.unpack_from("<I", hdr, 80)[0]
    esz = struct.unpack_from("<I", hdr, 84)[0]
    f.seek(part_lba * 512)
    for _ in range(num):
        e = f.read(esz)
        if len(e) < 128 or e[:16] == b"\0" * 16:
            continue
        name = e[56:128].decode("utf-16le").rstrip("\0")
        if name == "super":
            first, last = struct.unpack_from("<QQ", e, 32)
            print(first, last - first + 1)
            break
    else:
        sys.exit("no `super` partition in GPT")
PYEOF
)" || die "failed to parse GPT"
read -r SUPER_START SUPER_SECTORS <<<"$SUPER_INFO"
log "super partition: start LBA $SUPER_START, $SUPER_SECTORS sectors"

log "extracting super.img (sparse)"
"$DD" if="$IMG_DIR/system.img" of="$WORK/super.img" bs=512 \
    skip="$SUPER_START" count="$SUPER_SECTORS" conv=sparse status=none
[ -s "$WORK/super.img" ] || die "super.img extraction produced no data"

log "unpacking logical partition system"
mkdir -p "$WORK/lp"
"$LPUNPACK" -p system "$WORK/super.img" "$WORK/lp" >/dev/null 2>&1 \
    || die "lpunpack failed"
SYSIMG="$WORK/lp/system.img"
[ -f "$SYSIMG" ] || die "lpunpack did not produce $SYSIMG"

# --- 2. system/framework jars + framework-res + build.prop + fonts ----------
log "listing /system/framework"
FRAMEWORK_LIST="$("$DEBUGFS" -R 'ls -l /system/framework' "$SYSIMG" 2>/dev/null || true)"
[ -n "$FRAMEWORK_LIST" ] || die "could not list /system/framework"

extract_system_jar() { # <jar-name>
    local name="$1"
    debugfs_dump "$SYSIMG" "/system/framework/$name" "$OUT/jars/$name"
}

while read -r name; do
    [ -n "$name" ] || continue
    case "$name" in
        *.jar) extract_system_jar "$name" ;;
    esac
done < <(printf '%s\n' "$FRAMEWORK_LIST" | awk '{print $NF}' | grep -E '\.jar$' | sort -u)

debugfs_dump "$SYSIMG" "/system/framework/framework-res.apk" "$OUT/framework-res.apk"
debugfs_dump "$SYSIMG" "/system/build.prop" "$OUT/build.prop"

log "listing /system/fonts"
FONT_LIST="$("$DEBUGFS" -R 'ls -l /system/fonts' "$SYSIMG" 2>/dev/null || true)"
[ -n "$FONT_LIST" ] || die "could not list /system/fonts"
while read -r _inode mode rest; do
    name="${rest##* }"
    [ -n "$name" ] || continue
    case "$name" in
        *.ttf|*.otf|*.ttc) ;;
        *) continue ;;
    esac
    if printf '%s' "$mode" | grep -qE '^12[0-9]{4}$'; then
        # Symlink: preserve the link, do not try to dump it.
        target="$("$DEBUGFS" -R "stat /system/fonts/$name" "$SYSIMG" 2>/dev/null \
            | sed -n 's/^Fast link dest: "\(.*\)"$/\1/p')"
        if [ -n "$target" ]; then
            ln -sf "$target" "$OUT/fonts/$name"
            continue
        fi
        log "warning: could not resolve symlink /system/fonts/$name"
        continue
    fi
    debugfs_dump "$SYSIMG" "/system/fonts/$name" "$OUT/fonts/$name"
done < <(printf '%s\n' "$FONT_LIST" | tr -d '\r' | grep -E '\.(ttf|otf|ttc)$')
debugfs_dump "$SYSIMG" "/system/etc/fonts.xml" "$OUT/fonts/fonts.xml"

# Zygote preloaded-classes list (classes pre-initialised by the boot image).
if debugfs_dump_optional "$SYSIMG" "/system/etc/preloaded-classes" "$OUT/preloaded-classes"; then
    log "preloaded-classes: $(wc -l <"$OUT/preloaded-classes") classes"
else
    log "warning: /system/etc/preloaded-classes not present in image"
fi

# --- 3. APEX payloads (ART / i18n / conscrypt) ------------------------------
# Extract the javalib jars and classpath config from these APEXes.
APEX_NAMES=(
    com.google.android.art.apex
    com.android.i18n.apex
    com.android.runtime.apex
    com.google.android.conscrypt.apex
)
mkdir -p "$WORK/apex"
APEX_LIST="$("$DEBUGFS" -R 'ls -l /system/apex' "$SYSIMG" 2>/dev/null | awk '{print $NF}')"
for apex in "${APEX_NAMES[@]}"; do
    loc="/system/apex/$apex"
    if ! printf '%s\n' "$APEX_LIST" | grep -qx "$apex"; then
        log "skipping absent APEX $apex"
        continue
    fi
    log "APEX: $apex"
    debugfs_dump "$SYSIMG" "$loc" "$WORK/apex/$apex"
    payload="$WORK/apex/${apex%.apex}.img"
    if "$UNZIP" -l "$WORK/apex/$apex" 2>/dev/null | grep -q 'original_apex'; then
        "$UNZIP" -o -q "$WORK/apex/$apex" original_apex -d "$WORK/apex/x_${apex%.apex}" \
            || die "failed to unwrap original_apex from $apex"
        "$UNZIP" -o -q "$WORK/apex/x_${apex%.apex}/original_apex" apex_payload.img -d "$WORK/apex/x2_${apex%.apex}" \
            || die "failed to unwrap apex_payload.img from $apex"
        mv "$WORK/apex/x2_${apex%.apex}/apex_payload.img" "$payload"
    else
        "$UNZIP" -o -q "$WORK/apex/$apex" apex_payload.img -d "$WORK/apex/y_${apex%.apex}" \
            || die "failed to extract apex_payload.img from $apex"
        mv "$WORK/apex/y_${apex%.apex}/apex_payload.img" "$payload"
    fi

    # javalib/*.jar -> jars/
    JAVALIB="$("$DEBUGFS" -R 'ls -l /javalib' "$payload" 2>/dev/null || true)"
    [ -n "$JAVALIB" ] || { log "  (no /javalib in $apex)"; continue; }
    while read -r name; do
        [ -n "$name" ] || continue
        case "$name" in
            *.jar) debugfs_dump "$payload" "/javalib/$name" "$OUT/jars/$name" ;;
        esac
    done < <(printf '%s\n' "$JAVALIB" | awk '{print $NF}' | grep -E '\.jar$' | sort -u)

    # etc/classpaths/*.pb -> work/classpaths/
    CLASSROOM="$("$DEBUGFS" -R 'ls -l /etc/classpaths' "$payload" 2>/dev/null || true)"
    if [ -n "$CLASSROOM" ]; then
        while read -r name; do
            [ -n "$name" ] || continue
            case "$name" in
                *.pb) debugfs_dump "$payload" "/etc/classpaths/$name" "$WORK/classpaths/${apex%.apex}_$name" ;;
            esac
        done < <(printf '%s\n' "$CLASSROOM" | awk '{print $NF}' | grep -E '\.pb$' | sort -u)
    fi
done

# --- 4. BOOTCLASSPATH in real order -----------------------------------------
# ART APEX bootclasspath.pb first, then /system/etc/classpaths/bootclasspath.pb.
log "deriving BOOTCLASSPATH order"
debugfs_dump "$SYSIMG" "/system/etc/classpaths/bootclasspath.pb" "$WORK/classpaths/bootclasspath_pb"

"$PYTHON" - "$WORK/classpaths" "$OUT/jars" >"$OUT/BOOTCLASSPATH" <<'PYEOF' || die "failed to derive BOOTCLASSPATH"
import os, re, sys
cpdir, jardir = sys.argv[1], sys.argv[2]
# Source files in the order they appear on the real boot classpath.
sources = [
    os.path.join(cpdir, "com.google.android.art_bootclasspath.pb"),  # ART APEX boot jars
    os.path.join(cpdir, "bootclasspath_pb"),                         # /system/etc/classpaths
]
order = []
seen = set()
for src in sources:
    if not os.path.exists(src):
        continue
    data = open(src, "rb").read()
    for m in re.finditer(rb"(/(?:apex|system)/[^\x00]*?\.jar)", data):
        path = m.group(1).decode("ascii", "replace")
        base = os.path.basename(path)
        if base in seen:
            continue
        seen.add(base)
        order.append(base)
jars = set(os.listdir(jardir)) if os.path.isdir(jardir) else set()
missing = [b for b in order if b not in jars]
if missing:
    sys.stderr.write("missing jars for BOOTCLASSPATH: %s\n" % ", ".join(missing))
    sys.exit(1)
if not order:
    sys.stderr.write("no jars parsed from classpath configs\n")
    sys.exit(1)
sys.stdout.write("\n".join(order) + "\n")
PYEOF

# --- 5. unzip dex from every jar --------------------------------------------
log "unzipping dex from jars"
for jar in "$OUT/jars"/*.jar; do
    base="$(basename "$jar" .jar)"
    dest="$OUT/dex/$base"
    mkdir -p "$dest"
    if ! "$UNZIP" -o -q "$jar" 'classes*.dex' -d "$dest"; then
        rmdir "$dest" 2>/dev/null || true
        continue
    fi
    if [ -z "$(find "$dest" -name 'classes*.dex' -print -quit)" ]; then
        rmdir "$dest" 2>/dev/null || true
    fi
done

# --- 6. manifest.json --------------------------------------------------------
log "writing manifest.json"
"$PYTHON" - "$OUT" <<'PYEOF' || die "failed to write manifest.json"
import hashlib, json, os, sys
out = sys.argv[1]
files = []
for dirpath, dirnames, filenames in os.walk(out):
    dirnames[:] = [d for d in dirnames if d != ".work"]
    for fn in sorted(filenames):
        if fn == "manifest.json":
            continue
        full = os.path.join(dirpath, fn)
        rel = os.path.relpath(full, out)
        h = hashlib.sha256()
        with open(full, "rb") as f:
            for chunk in iter(lambda: f.read(1 << 20), b""):
                h.update(chunk)
        files.append({"path": rel, "size": os.path.getsize(full), "sha256": h.hexdigest()})
files.sort(key=lambda e: e["path"])
manifest = {
    "fidelity": "art-reference",
    "description": "Android 14 (API 34) boot classpath jars + system files extracted from the SDK system image",
    "android_api": 34,
    "abi": "x86_64",
    "source_image": "system-images/android-34/google_apis/x86_64/system.img",
    "bootclasspath": open(os.path.join(out, "BOOTCLASSPATH")).read().split(),
    "file_count": len(files),
    "total_bytes": sum(e["size"] for e in files),
    "files": files,
}
with open(os.path.join(out, "manifest.json"), "w") as f:
    json.dump(manifest, f, indent=2, sort_keys=True)
    f.write("\n")
print("manifest: %d files, %.2f MiB" % (len(files), sum(e["size"] for e in files) / 1048576.0))
PYEOF

# --- 7. summary --------------------------------------------------------------
dex_count=0
dex_bytes=0
while IFS= read -r d; do
    [ -n "$d" ] || continue
    dex_count=$((dex_count + 1))
    dex_bytes=$((dex_bytes + $(stat -c '%s' "$d")))
done < <(find "$OUT/dex" -name 'classes*.dex' | sort)

log "summary:"
printf '    %-18s %s\n' "output"        "$OUT"
printf '    %-18s %s\n' "jars"          "$(find "$OUT/jars" -name '*.jar' | wc -l) ($(du -sh "$OUT/jars" | cut -f1))"
printf '    %-18s %s\n' "dex files"     "$dex_count ($(numfmt --to=iec "$dex_bytes" 2>/dev/null || echo "$dex_bytes B"))"
printf '    %-18s %s\n' "fonts"         "$(find "$OUT/fonts" -type f | wc -l)"
printf '    %-18s %s\n' "total size"    "$(du -sh --exclude=.work "$OUT" | cut -f1) (peak work +$(du -sh "$WORK" 2>/dev/null | cut -f1))"
printf '    %-18s %s\n' "BOOTCLASSPATH" "$(paste -sd, "$OUT/BOOTCLASSPATH")"
