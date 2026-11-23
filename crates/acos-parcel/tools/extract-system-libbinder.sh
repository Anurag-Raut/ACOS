#!/usr/bin/env bash
#
# extract-system-libbinder.sh — extract the SYSTEM (not VNDK) libbinder.so and
# libandroid_runtime.so from the installed API-34 system image into
# .cache/parcel-evidence/ using only unprivileged, already-installed tools.
#
# This is a read-only-evidence helper for acos-parcel (T-02 interface-token
# semantics). It intentionally mirrors the GPT -> super -> lpunpack -> debugfs
# method of tools/fetch/extract-bootclasspath.sh without modifying it. Everything is
# written under .cache/parcel-evidence/ (gitignored); the multi-GB intermediates
# (super.img, system.img) live in .cache/parcel-evidence/.work and are removed
# on exit, including on failure.
#
# Tools are the same set the bootclasspath extractor already relies on:
# dd, lpunpack, debugfs (all unprivileged; no mounts, no sudo, no network).
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
SDK="${ANDROID_HOME:-${ANDROID_SDK_ROOT:-/home/Qui/Android/Sdk}}"
IMAGE_REL="${ANDROID_IMAGE_REL:-system-images/android-34/google_apis/x86_64}"
IMG_DIR="$SDK/$IMAGE_REL"
OUT="$ROOT/.cache/parcel-evidence"
WORK="$OUT/.work"
FORCE=0
[ "${1:-}" = "--force" ] && FORCE=1

log() { printf '[extract-system-libbinder] %s\n' "$*"; }
die() { printf '[extract-system-libbinder] error: %s\n' "$*" >&2; exit 1; }

need() {
    local t="$1" p
    p="$(command -v "$t" 2>/dev/null || true)"
    [ -n "$p" ] || die "required tool not found on PATH: $t"
    printf '%s' "$p"
}
DD="$(need dd)"
LPUNPACK="$(need lpunpack)"
DEBUGFS="$(need debugfs)"
PYTHON="$(need python3)"
SHA256="$(need sha256sum)"

[ -f "$IMG_DIR/system.img" ] || die "system image not found: $IMG_DIR/system.img"

# The google_apis x86_64 image only ships 64-bit libraries; 32-bit paths are
# probed and skipped when absent.
TARGETS=(
    "/system/lib64/libbinder.so"
    "/system/lib64/libandroid_runtime.so"
    "/system/lib/libbinder.so"
    "/system/lib/libandroid_runtime.so"
)
REQUIRED=("/system/lib64/libbinder.so")

if [ "$FORCE" -ne 1 ]; then
    all_present=1
    for t in "${REQUIRED[@]}"; do
        [ -f "$OUT/${t#/system/}" ] || all_present=0
    done
    if [ "$all_present" -eq 1 ]; then
        log "already extracted under $OUT (use --force to redo)"
        exit 0
    fi
fi

cleanup() { rm -rf "$WORK"; }
trap cleanup EXIT
rm -rf "$OUT"
mkdir -p "$OUT" "$WORK"

# --- 1. GPT -> super -> system.img (same as extract-bootclasspath.sh) --------
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

# --- 2. dump the requested libraries ----------------------------------------
parseable() { # <abs-path-in-image> -> 0 if debugfs can stat it
    local path="$1"
    "$DEBUGFS" -R "stat $path" "$SYSIMG" 2>/dev/null | grep -q '^Inode:'
}

dump() { # <abs-path-in-image>
    local path="$1"
    local rel="${path#/system/}"
    mkdir -p "$(dirname "$OUT/$rel")"
    "$DEBUGFS" -R "dump $path $OUT/$rel" "$SYSIMG" >/dev/null 2>&1 \
        || die "debugfs failed to dump $path"
    [ -s "$OUT/$rel" ] || die "debugfs produced no data for $path"
    log "dumped $path -> $rel ($(stat -c%s "$OUT/$rel") bytes)"
}

DUMPED=()
for t in "${TARGETS[@]}"; do
    if parseable "$t"; then
        dump "$t"
        DUMPED+=("$t")
    else
        log "absent, skipping $t"
    fi
done
[ "${#DUMPED[@]}" -gt 0 ] || die "no target libraries found"

# --- 3. record hashes, then drop the intermediates ---------------------------
{
    echo "# acos-parcel system-image evidence"
    echo
    echo "Source image: \`$IMG_DIR/system.img\` (read-only)."
    echo
    echo "| extracted file | bytes | sha256 |"
    echo "|---|---:|---|"
    for t in "${DUMPED[@]}"; do
        rel="${t#/system/}"
        printf '| `%s` | %s | `%s` |\n' "$rel" "$(stat -c%s "$OUT/$rel")" "$("$SHA256" "$OUT/$rel" | cut -d' ' -f1)"
    done
} >"$OUT/EVIDENCE.md"

log "hashes recorded in $OUT/EVIDENCE.md"
log "done"
