#!/usr/bin/env bash
#
# GLSPIKE: stage a software GLES provider for the guest (opt-in, default off).
#
# The guest image ships the framework's EGL *loader* but no driver, so the first
# HARDWARE-bitmap draw aborts in libEGL ("couldn't find an OpenGL ES
# implementation"). libEGL's own search path is the seam
# (frameworks/native/opengl/libs/EGL/Loader.cpp, android-14.0.0_r75):
#
#   attempt_to_load_system_driver(cnx, ro.hardware.egl, true)
#     -> /vendor/lib64/egl/libEGL_<tag>.so
#        /vendor/lib64/egl/libGLESv1_CM_<tag>.so
#        /vendor/lib64/egl/libGLESv2_<tag>.so
#
# This script installs a provider directory
#
#   <sysroot>/gles/<tag>/
#     manifest.json                 tag, provider, source, version, licence,
#                                   {path, sha256, size} per file
#     lib64/egl/libEGL_<tag>.so     findLibrary's directory (libEGL_<tag>.so,
#                                   libGLESv1_CM_<tag>.so, libGLESv2_<tag>.so)
#     lib64/libEGL_<tag>.so         the same inodes, for the driver's DT_NEEDED
#                                   (the image's ld.config has no
#                                   /vendor/${LIB}/egl in the default namespace)
#
# and `acos-art` (crates/acos-art/src/gles.rs) bind-mounts lib64 at
# /vendor/lib64/egl and sets ro.hardware.egl=<tag> when ACOS_GUEST_GLES=<tag>.
# It verifies every file against the manifest on every launch with the flag on.
# lib64/ is bound at /vendor/lib64, so `lib64/egl/libEGL_<tag>.so` (what the
# loader opens) and `lib64/libGLESv2_<tag>.so` (what the driver NEEDs) both
# resolve with one mount.
#
# Usage:
#   tools/art-gles.sh --tag angle --from-emulator-vendor <vendor.img|dir> \
#       [--provider NAME] [--version V] [--licence SPDX] [--source URL] \
#       [--sysroot DIR]
#   tools/art-gles.sh --tag mesa --from-dir <dir with libEGL_mesa.so ...> ...
#
# The provider lives next to the guest's shared, read-only inputs — the *sysroot*
# (`<sysroot>/gles/<tag>`, the same add-on path as `<sysroot>/shim/`), which is
# NOT per-run: `run-exclusive.sh` gives every run a fresh `ACOS_ART_DATA`, so a
# provider under the data dir would vanish with the run. The default --sysroot
# is `$ACOS_ART_SYSROOT`, else the repo's `.cache/art/sysroot`; the launcher
# finds it there (`crates/acos-art/src/gles.rs`), or under
# `$ACOS_GUEST_GLES_DIR` when an arm wants a private copy. A run without the
# flag never looks, so the add-on is invisible by default.
#
# The emulator source is the *guest* vendor image (`system-images/.../vendor.img`,
# an ext4 filesystem inside a GPT partition): its /lib64/egl holds bionic-built
# drivers. Sizes, hashes and the licence are recorded in the manifest so the
# report can cite them; this script writes no GL code.
set -euo pipefail

WT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TAG=""
FROM_DIR=""
FROM_EMULATOR_VENDOR=""
PROVIDER=""
VERSION=""
LICENCE=""
SOURCE=""
SYSROOT="${ACOS_ART_SYSROOT:-$WT/.cache/art/sysroot}"
DRIVER_FILES=(libEGL libGLESv1_CM libGLESv2)

die() { echo "art-gles: $*" >&2; exit 2; }

while [ $# -gt 0 ]; do
  case "$1" in
    --tag) TAG="$2"; shift 2 ;;
    --from-dir) FROM_DIR="$2"; shift 2 ;;
    --from-emulator-vendor) FROM_EMULATOR_VENDOR="$2"; shift 2 ;;
    --provider) PROVIDER="$2"; shift 2 ;;
    --version) VERSION="$2"; shift 2 ;;
    --licence) LICENCE="$2"; shift 2 ;;
    --source) SOURCE="$2"; shift 2 ;;
    --sysroot) SYSROOT="$2"; shift 2 ;;
    -h|--help) sed -n '2,40p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'; exit 0 ;;
    *) die "unknown argument: $1" ;;
  esac
done

[ -n "$TAG" ] || die "--tag is required (it is the ro.hardware.egl value and the provider name)"
case "$TAG" in */*|.|..) die "--tag must be a plain name, not a path: $TAG" ;; esac
[ -n "$FROM_DIR" ] || [ -n "$FROM_EMULATOR_VENDOR" ] || \
  die "--from-dir or --from-emulator-vendor is required"

STAGE="$SYSROOT/gles/$TAG"
EGL="$STAGE/lib64/egl"
SCRATCH="$WT/.cache/glspike-scratch/art-gles.$$"
trap 'rm -rf "$SCRATCH"' EXIT

need() { command -v "$1" >/dev/null 2>&1 || die "missing tool: $1"; }
need sha256sum
need python3

# ---- source: a directory of driver .so files, or the emulator's vendor image --
if [ -n "$FROM_EMULATOR_VENDOR" ]; then
  need debugfs
  src_img="$FROM_EMULATOR_VENDOR"
  if [ -d "$src_img" ]; then
    SRC_DIR="$src_img/lib64/egl"
  else
    mkdir -p "$SCRATCH"
    # The SDK system images are GPT-partitioned ext4: partition 1 is `vendor`.
    # Take the partition's own offset/size from the GPT header rather than
    # guessing a sector count.
    read -r start sectors < <(python3 - "$src_img" <<'PY'
import struct, sys
p = sys.argv[1]
with open(p, 'rb') as f:
    f.seek(512)
    hdr = f.read(92)
    if hdr[:8] != b'EFI PART':
        sys.exit('not a GPT disk image')
    part_lba, nparts, esize = struct.unpack_from('<QII', hdr, 72)
    f.seek(part_lba * 512)
    e = f.read(esize)
first = None
with open(p, 'rb') as f:
    f.seek(part_lba * 512)
    for _ in range(nparts):
        e = f.read(esize)
        if e[:16] != b'\0' * 16:
            a, b = struct.unpack_from('<QQ', e, 32)
            first = (a, b - a + 1)
            break
if first is None:
    sys.exit('no GPT partition')
print(first[0], first[1])
PY
)
    dd if="$src_img" of="$SCRATCH/vendor-part.img" bs=512 skip="$start" count="$sectors" status=none
    SRC_DIR="$SCRATCH/from-img"
    mkdir -p "$SRC_DIR"
    for base in "${DRIVER_FILES[@]}"; do
      name="${base}_${TAG}.so"
      debugfs -R "dump /lib64/egl/$name $SRC_DIR/$name" "$SCRATCH/vendor-part.img" 2>/dev/null || true
    done
  fi
else
  SRC_DIR="$FROM_DIR"
fi

[ -d "$SRC_DIR" ] || die "source EGL directory not found: $SRC_DIR"

# ---- what a provider must carry ----------------------------------------------
# libEGL_<tag> is mandatory (the loader's first candidate); the GLES libs are
# what libEGL dlopens next (`SYSTEM_LIB_PATH "/libGLESv1_CM.so"` wrappers go
# through the driver's own exports, but a driver that only ships libEGL_* is
# not what any Android GL driver does).
missing=()
for base in "${DRIVER_FILES[@]}"; do
  [ -f "$SRC_DIR/${base}_${TAG}.so" ] || missing+=("${base}_${TAG}.so")
done
if [ ${#missing[@]} -gt 0 ]; then
  echo "art-gles: $SRC_DIR lacks: ${missing[*]}" >&2
  echo "art-gles: available: $(ls "$SRC_DIR" 2>/dev/null | tr '\n' ' ')" >&2
  exit 2
fi

# ---- install (atomic publish) -------------------------------------------------
[ -d "$SYSROOT" ] || die "sysroot not found: $SYSROOT (extract it with tools/art-sysroot.sh)"
mkdir -p "$SYSROOT/gles"
tmp="$STAGE.tmp.$$"
rm -rf "$tmp"
mkdir -p "$tmp/lib64/egl"
for base in "${DRIVER_FILES[@]}"; do
  cp "$SRC_DIR/${base}_${TAG}.so" "$tmp/lib64/egl/"
  # The same file again one level up, as a hardlink: the guest's generated
  # ld.config.txt gives the default namespace `/vendor/${LIB}` but not
  # `/vendor/${LIB}/egl` (there is no vendor partition and no sphal namespace),
  # so a driver that NEEDs its GLES lib must be resolvable from /vendor/lib64.
  # One inode, two names: no extra disk, and the launcher binds lib64/ once.
  ln "$tmp/lib64/egl/${base}_${TAG}.so" "$tmp/lib64/${base}_${TAG}.so"
done

python3 - "$tmp" "$TAG" "${PROVIDER:-$TAG}" "${SOURCE:-$SRC_DIR}" \
         "${VERSION:-unknown}" "${LICENCE:-unknown}" <<'PY'
import hashlib, json, os, sys
stage, tag, provider, source, version, licence = sys.argv[1:7]
files = []
for root, _dirs, names in os.walk(os.path.join(stage, 'lib64')):
    for name in sorted(names):
        p = os.path.join(root, name)
        h = hashlib.sha256()
        with open(p, 'rb') as f:
            for chunk in iter(lambda: f.read(1 << 20), b''):
                h.update(chunk)
        files.append({
            'path': os.path.relpath(p, stage).replace(os.sep, '/'),
            'sha256': h.hexdigest(),
            'size': os.path.getsize(p),
        })
files.sort(key=lambda f: f['path'])
manifest = {
    'tag': tag,
    'provider': provider,
    'source': source,
    'version': version,
    'licence': licence,
    'files': files,
}
with open(os.path.join(stage, 'manifest.json'), 'w') as f:
    json.dump(manifest, f, indent=2, sort_keys=True)
    f.write('\n')
print(f"art-gles: {len(files)} file(s), {sum(f['size'] for f in files)} bytes")
for f in files:
    print(f"art-gles:   {f['path']}  {f['size']}  {f['sha256']}")
PY

rm -rf "$STAGE"
mv "$tmp" "$STAGE"
echo "art-gles: installed $STAGE  (run with ACOS_GUEST_GLES=$TAG)"
