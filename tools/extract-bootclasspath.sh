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
#   SYSTEMSERVERCLASSPATH               same for the system-server classpath
#   build.prop                          /system/build.prop from the image
#   fonts/                              /system/fonts/*.{ttf,otf,ttc} + fonts.xml
#   framework-res.apk                   /system/framework/framework-res.apk
#   overlays/<name>.apk                 static RROs over `android` (product/,
#                                       system_ext/ `overlay/` partitions)
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
if [ "$FORCE" -ne 1 ] && [ -f "$OUT/manifest.json" ] && [ -f "$OUT/BOOTCLASSPATH" ] \
    && [ -f "$OUT/SYSTEMSERVERCLASSPATH" ]; then
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
mkdir -p "$OUT/jars" "$OUT/dex" "$OUT/fonts" "$OUT/overlays" "$WORK"

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

# --- 2b. static resource overlays (RROs) ------------------------------------
# A **static** RRO (`<overlay android:isStatic="true">`) cannot be enabled or
# disabled at runtime, so the framework always applies it. On the Google system
# image `com.google.android.overlay.pixelconfigcommon` targets `android` and
# flips `@bool/config_buttonTextAllCaps` from `true` (framework-res) to `false`,
# which is why reference Button labels are mixed case and a Button measures
# `text + 12dp/side` instead of the all-caps width.
#
# Overlays live on the `product` and `system_ext` dynamic partitions, not on
# `system`, so those are unpacked too. Only static overlays are extracted: a
# dynamic RRO is off unless `OverlayManager` enables it, and ACOS has no
# overlay service to ask.
log "unpacking product + system_ext for static overlays"
mkdir -p "$OUT/overlays"
for part in product system_ext; do
    "$LPUNPACK" -p "$part" "$WORK/super.img" "$WORK/lp" >/dev/null 2>&1 || {
        log "  ($part: lpunpack failed; skipping)"
        continue
    }
    partimg="$WORK/lp/$part.img"
    [ -f "$partimg" ] || { log "  ($part: no image; skipping)"; continue; }
    for apk in $("$DEBUGFS" -R 'ls -l /overlay' "$partimg" 2>/dev/null \
            | awk '{print $NF}' | grep -E '\.apk$' | sort); do
        debugfs_dump_optional "$partimg" "/overlay/$apk" "$WORK/ov/$apk" || continue
        # `aapt2 dump xmltree --file AndroidManifest.xml` is not available in
        # every SDK, so read the `isStatic`/`targetPackage` attributes from the
        # binary manifest with a tiny AXML scan instead.
        if "$PYTHON" - "$WORK/ov/$apk" <<'PYEOF'
import struct, sys, zipfile

def pool(data, off):
    _, _, size, count, _, flags, strings_start, _ = struct.unpack_from('<HHIIIIII', data, off)
    utf8 = bool(flags & 0x100)
    offs = [struct.unpack_from('<I', data, off + 28 + 4 * i)[0] for i in range(count)]
    out = []
    for o in offs:
        p = off + strings_start + o
        if utf8:
            n = data[p]
            p += 2 if n & 0x80 else 1
            n = data[p]
            if n & 0x80:
                n = ((n & 0x7F) << 8) | data[p + 1]
                p += 2
            else:
                p += 1
            out.append(data[p:p + n].decode('utf-8', 'replace'))
        else:
            n = struct.unpack_from('<H', data, p)[0]
            p += 2
            out.append(data[p:p + 2 * n].decode('utf-16le', 'replace'))
    return out

try:
    with zipfile.ZipFile(sys.argv[1]) as z:
        data = z.read('AndroidManifest.xml')
except Exception:
    sys.exit(1)
if struct.unpack_from('<H', data, 0)[0] != 0x0003:
    sys.exit(1)
strings = pool(data, 8)
pos = struct.unpack_from('<H', data, 2)[0]
static = target = None
while pos + 8 <= len(data):
    ctype, hsize, csize = struct.unpack_from('<HHI', data, pos)
    if csize < 8:
        break
    if ctype == 0x0102:  # START_ELEMENT
        ext = pos + hsize
        name = strings[struct.unpack_from('<I', data, ext + 4)[0]]
        attr_start, attr_size, attr_count = struct.unpack_from('<HHH', data, ext + 8)
        if name == 'overlay':
            for i in range(attr_count):
                a = ext + attr_start + i * attr_size
                aname = strings[struct.unpack_from('<I', data, a + 4)[0]]
                raw = struct.unpack_from('<i', data, a + 8)[0]
                dtype = data[a + 15]
                if aname == 'isStatic' and dtype == 0x12:
                    static = struct.unpack_from('<I', data, a + 16)[0] != 0
                if aname == 'targetPackage' and raw >= 0:
                    target = strings[raw]
    pos += csize
sys.exit(0 if (static and target == 'android') else 1)
PYEOF
        then
            cp "$WORK/ov/$apk" "$OUT/overlays/$apk"
            log "  overlay (static, target=android): $part/overlay/$apk"
        fi
    done
done
[ -n "$(ls -A "$OUT/overlays" 2>/dev/null)" ] \
    && log "static overlays: $(ls "$OUT/overlays" | tr '\n' ' ')" \
    || log "warning: no static android-target overlays extracted"

# --- 3. APEX payloads --------------------------------------------------------
# Every APEX may contribute a classpath fragment (`bootclasspath.pb` /
# `systemserverclasspath.pb`) and the `/javalib` jars those fragments
# reference. Extract them all instead of a hard-coded list, so the derivation
# below merges the real fragments (`framework-connectivity.jar` from the
# tethering APEX, `framework-permission.jar`, ...). `framework.jar` references
# classes defined in those sibling jars, so a boot classpath without them is
# incomplete.
mkdir -p "$WORK/apex"
APEX_LIST="$("$DEBUGFS" -R 'ls -l /system/apex' "$SYSIMG" 2>/dev/null \
    | awk '{print $NF}' | grep -E '\.apex$' | sort)"
[ -n "$APEX_LIST" ] || die "could not list /system/apex"
while read -r apex; do
    [ -n "$apex" ] || continue
    loc="/system/apex/$apex"
    log "APEX: $apex"
    debugfs_dump "$SYSIMG" "$loc" "$WORK/apex/$apex"
    payload="$WORK/apex/${apex%.apex}.img"
    if "$UNZIP" -l "$WORK/apex/$apex" 2>/dev/null | grep -q 'original_apex'; then
        "$UNZIP" -o -q "$WORK/apex/$apex" original_apex -d "$WORK/apex/x_${apex%.apex}" 2>/dev/null || true
        "$UNZIP" -o -q "$WORK/apex/x_${apex%.apex}/original_apex" apex_payload.img -d "$WORK/apex/x2_${apex%.apex}" 2>/dev/null || true
        mv "$WORK/apex/x2_${apex%.apex}/apex_payload.img" "$payload" 2>/dev/null || true
    else
        "$UNZIP" -o -q "$WORK/apex/$apex" apex_payload.img -d "$WORK/apex/y_${apex%.apex}" 2>/dev/null || true
        mv "$WORK/apex/y_${apex%.apex}/apex_payload.img" "$payload" 2>/dev/null || true
    fi
    if [ ! -f "$payload" ]; then
        log "  (no apex_payload.img; skipping)"
        continue
    fi

    # javalib/*.jar -> jars/
    JAVALIB="$("$DEBUGFS" -R 'ls -l /javalib' "$payload" 2>/dev/null || true)"
    if [ -n "$JAVALIB" ]; then
        while read -r name; do
            [ -n "$name" ] || continue
            case "$name" in
                *.jar) debugfs_dump "$payload" "/javalib/$name" "$OUT/jars/$name" ;;
            esac
        done < <(printf '%s\n' "$JAVALIB" | awk '{print $NF}' | grep -E '\.jar$' | sort -u)
    else
        log "  (no /javalib in $apex)"
    fi

    # etc/icu/*.dat -> apex/<apex>/etc/icu/ (ICU data: i18n icudt72l.dat,
    # tzdata *.dat). The VFS maps `/system/apex/<apex>/**` here, so a forced
    # re-extraction keeps `ICUBinary.getDataFromFile` working.
    ICU_LIST="$("$DEBUGFS" -R 'ls -l /etc/icu' "$payload" 2>/dev/null || true)"
    if [ -n "$ICU_LIST" ]; then
        while read -r name; do
            [ -n "$name" ] || continue
            case "$name" in
                *.dat) debugfs_dump "$payload" "/etc/icu/$name" "$OUT/apex/${apex%.apex}/etc/icu/$name" ;;
            esac
        done < <(printf '%s\n' "$ICU_LIST" | awk '{print $NF}' | grep -E '\.dat$' | sort -u)
    fi

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
done < <(printf '%s\n' "$APEX_LIST")

# --- 4. BOOTCLASSPATH / SYSTEMSERVERCLASSPATH --------------------------------
# `derive_classpath` merges the per-APEX and system classpath configs. The ART
# APEX holds the core Java boot jars; `/system/etc/classpaths` holds the
# framework jars; every other APEX contributes its own fragment
# (`framework-connectivity.jar` from tethering, `framework-permission.jar`,
# ...). Merge every `*_<kind>.pb` fragment rather than a hard-coded source list
# so a jar added by a future APEX is picked up from the image itself.
log "deriving classpath order"
debugfs_dump "$SYSIMG" "/system/etc/classpaths/bootclasspath.pb" "$WORK/classpaths/bootclasspath_pb"
debugfs_dump_optional "$SYSIMG" "/system/etc/classpaths/systemserverclasspath.pb" \
    "$WORK/classpaths/systemserverclasspath_pb" || true

emit_classpath() { # <kind> <output-file>
    "$PYTHON" - "$WORK/classpaths" "$OUT/jars" "$1" "$2" <<'PYEOF' || die "failed to derive $1"
import os, re, sys
cpdir, jardir, kind, outfile = sys.argv[1:5]
system_file = os.path.join(cpdir, kind + "_pb")
if kind == "bootclasspath":
    # The core-oj/... jars and conscrypt come first; the rest are appended.
    preferred = [
        os.path.join(cpdir, "com.google.android.art_bootclasspath.pb"),       # ART APEX
        os.path.join(cpdir, "com.google.android.conscrypt_bootclasspath.pb"), # conscrypt APEX
        system_file,                                                          # /system/etc/classpaths
    ]
else:
    preferred = [system_file]
rest = sorted(f for f in os.listdir(cpdir) if f.endswith("_" + kind + ".pb"))
sources = preferred + [os.path.join(cpdir, f) for f in rest]
order, seen = [], set()
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
    sys.stderr.write("missing jars for %s: %s\n" % (kind, ", ".join(missing)))
    sys.exit(1)
if not order:
    sys.stderr.write("no jars parsed from %s configs\n" % kind)
    sys.exit(1)
with open(outfile, "w") as f:
    f.write("\n".join(order) + "\n")
sys.stdout.write("%s: %d jars\n" % (kind, len(order)))
PYEOF
}
emit_classpath bootclasspath "$OUT/BOOTCLASSPATH"
emit_classpath systemserverclasspath "$OUT/SYSTEMSERVERCLASSPATH"
log "BOOTCLASSPATH: $(paste -sd, "$OUT/BOOTCLASSPATH")"

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
    "systemserverclasspath": open(os.path.join(out, "SYSTEMSERVERCLASSPATH")).read().split(),
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
printf '    %-18s %s\n' "SYSTEMSERVERCLASSPATH" "$(paste -sd, "$OUT/SYSTEMSERVERCLASSPATH")"
