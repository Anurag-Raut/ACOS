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
# /vendor/lib64 and sets ro.hardware.egl=<egl-tag> when ACOS_GUEST_GLES=<tag>.
#
# GLSPIKE2 adds the Vulkan ICD an ANGLE build needs to *initialize* (ANGLE has
# no software rasteriser of its own; `EGL_NOT_INITIALIZED` in GLSPIKE was the
# missing ICD). A provider may carry
#
#   lib64/hw/vulkan.<vulkan-tag>.so   the ICD at its canonical HAL path
#   lib64/vulkan.<vulkan-tag>.so      the same file (hardlink) by the BARE name
#                                     the image's libvulkan.so actually
#                                     dlopens (`vulkan.<tag>.so` through
#                                     libvndksupport, resolved over
#                                     /vendor/lib64; GLSPIKE4). Named by
#                                     ro.hardware.vulkan
#
# and then the manifest also carries vulkan_tag=<vulkan-tag>, which the launcher
# turns into ro.hardware.vulkan. `--egl-tag` lets the provider directory be a
# label (`angle-swiftshader`) while the EGL library names stay `..._angle.so`.
#
# GPUSPIKE (host GPU, `mesa-host`): a provider may also carry
#
#   --extra-lib FILE     staged at lib64/<name> (the driver's DT_NEEDED closure:
#                        libgallium_dri.so, libLLVM, libdrm*, libgbm_mesa, ...)
#   --extra-hw FILE      staged at lib64/hw/<name> (HAL modules: gralloc.<tag>.so,
#                        the passthrough mapper impl libhidl globs by prefix)
#   --prop KEY=VALUE     manifest `props`, written into the guest property table
#                        (ro.hardware.gralloc=gbm, gralloc.gbm.device=...)
#   --render-node PATH   manifest `render_node` (/dev/dri/renderD<N>): the
#                        launcher binds that one node read-write into the
#                        sandbox, only when this provider is selected
#
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
#   tools/art-gles.sh --tag angle-swiftshader --from-dir <dir> \
#       --egl-tag angle --vulkan-icd <vulkan.pastel.so> --vulkan-tag pastel ...
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
EGL_TAG=""
VULKAN_ICD=""
VULKAN_TAG="pastel"
FROM_DIR=""
EXTRA_LIBS=()
EXTRA_HW=()
PROPS=()
RENDER_NODE=""
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
    --egl-tag) EGL_TAG="$2"; shift 2 ;;
    --vulkan-icd) VULKAN_ICD="$2"; shift 2 ;;
    --vulkan-tag) VULKAN_TAG="$2"; shift 2 ;;
    --from-dir) FROM_DIR="$2"; shift 2 ;;
    --extra-lib) EXTRA_LIBS+=("$2"); shift 2 ;;
    --extra-hw) EXTRA_HW+=("$2"); shift 2 ;;
    --prop) PROPS+=("$2"); shift 2 ;;
    --render-node) RENDER_NODE="$2"; shift 2 ;;
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

[ -n "$TAG" ] || die "--tag is required (the provider name; the ro.hardware.egl value is --egl-tag, default the same)"
case "$TAG" in */*|.|..) die "--tag must be a plain name, not a path: $TAG" ;; esac
EGL_TAG="${EGL_TAG:-$TAG}"
case "$EGL_TAG" in */*|.|..|"") die "--egl-tag must be a plain name, not a path: $EGL_TAG" ;; esac
case "$VULKAN_TAG" in */*|.|..|"") die "--vulkan-tag must be a plain name, not a path: $VULKAN_TAG" ;; esac
VT_ARG=""
if [ -n "$VULKAN_ICD" ]; then
  [ -f "$VULKAN_ICD" ] || die "--vulkan-icd is not a file: $VULKAN_ICD"
  VT_ARG="$VULKAN_TAG"
fi
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
      name="${base}_${EGL_TAG}.so"
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
  [ -f "$SRC_DIR/${base}_${EGL_TAG}.so" ] || missing+=("${base}_${EGL_TAG}.so")
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
if [ -n "$VULKAN_ICD" ]; then
  # The ICD is used only through its own path (`/vendor/lib64/hw/...`), so a
  # copy is enough; it is not dlopened by soname.
  mkdir -p "$tmp/lib64/hw"
  cp "$VULKAN_ICD" "$tmp/lib64/hw/vulkan.${VULKAN_TAG}.so"
  # GLSPIKE4: NOT only through its HAL path. The image's libvulkan.so loads the
  # ICD with libvndksupport's `android_load_sphal_library("vulkan.<tag>.so")` --
  # the BARE name -- and with no sphal namespace that is a default-namespace
  # dlopen over `/vendor/${LIB}` (not `/vendor/${LIB}/hw`). Without this second
  # name the guest logs `vndksupport: Could not load vulkan.<tag>.so from
  # default namespace`, vkCreateInstance fails and ANGLE's eglInitialize reports
  # EGL_NOT_INITIALIZED while libEGL still returns EGL_TRUE. One inode.
  ln "$tmp/lib64/hw/vulkan.${VULKAN_TAG}.so" "$tmp/lib64/vulkan.${VULKAN_TAG}.so"
fi
for f in ${EXTRA_LIBS[@]+"${EXTRA_LIBS[@]}"}; do
  [ -f "$f" ] || die "--extra-lib is not a file: $f"
  cp "$f" "$tmp/lib64/$(basename "$f")"
done
if [ ${#EXTRA_HW[@]} -gt 0 ]; then
  mkdir -p "$tmp/lib64/hw"
  for f in "${EXTRA_HW[@]}"; do
    [ -f "$f" ] || die "--extra-hw is not a file: $f"
    cp "$f" "$tmp/lib64/hw/$(basename "$f")"
  done
fi
for base in "${DRIVER_FILES[@]}"; do
  cp "$SRC_DIR/${base}_${EGL_TAG}.so" "$tmp/lib64/egl/"
  # The same file again one level up, as a hardlink: the guest's generated
  # ld.config.txt gives the default namespace `/vendor/${LIB}` but not
  # `/vendor/${LIB}/egl` (there is no vendor partition and no sphal namespace),
  # so a driver that NEEDs its GLES lib must be resolvable from /vendor/lib64.
  # One inode, two names: no extra disk, and the launcher binds lib64/ once.
  ln "$tmp/lib64/egl/${base}_${EGL_TAG}.so" "$tmp/lib64/${base}_${EGL_TAG}.so"
done

python3 - "$tmp" "$EGL_TAG" "${PROVIDER:-$TAG}" "${SOURCE:-$SRC_DIR}" \
         "${VERSION:-unknown}" "${LICENCE:-unknown}" \
         "$VT_ARG" "$RENDER_NODE" "${PROPS[@]+"${PROPS[@]}"}" <<'PY'
import hashlib, json, os, sys
stage, tag, provider, source, version, licence, vulkan_tag, render_node = sys.argv[1:9]
props = {}
for kv in sys.argv[9:]:
    k, _, v = kv.partition('=')
    props[k] = v
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
if vulkan_tag:
    manifest['vulkan_tag'] = vulkan_tag
if props:
    manifest['props'] = props
if render_node:
    manifest['render_node'] = render_node
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
