#!/usr/bin/env bash
#
# fetch-qemu-aarch64.sh -- extract qemu-aarch64 user-locally (no root).
#
# ACOS requires no `sudo` and no system install. This downloads the Arch
# `qemu-user` package and extracts `usr/bin/qemu-aarch64` (plus its host
# libraries are taken from the running Arch host) under `<repo>/.cache/qemu-user`.
# Nothing is installed and no system path is touched.
#
# The binary is the transport proven in results/arm64/W190A.md.
#
# Usage:
#   tools/fetch-qemu-aarch64.sh
#
# Env overrides:
#   ACOS_QEMU_VERSION   package version (default 11.1.3-1)
#   ACOS_QEMU_SHA256    package sha256 (must match the version)
#   ACOS_QEMU_DIR       extraction root (default <repo>/.cache/qemu-user)
set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
VERSION="${ACOS_QEMU_VERSION:-11.1.3-1}"
SHA="${ACOS_QEMU_SHA256:-4f2f543840e6b970b50aaa42fa327f69d371dfa4b71e2030d5bf47e47e614656}"
DEST="${ACOS_QEMU_DIR:-$ROOT/.cache/qemu-user}"
PKG_URL="https://geo.mirror.pkgbuild.com/extra/os/x86_64/qemu-user-${VERSION}-x86_64.pkg.tar.zst"

log() { printf '[qemu-aarch64] %s\n' "$*"; }
die() { printf '[qemu-aarch64] ERROR: %s\n' "$*" >&2; exit 1; }

if [ -x "$DEST/usr/bin/qemu-aarch64" ]; then
    if "$DEST/usr/bin/qemu-aarch64" --version >/dev/null 2>&1; then
        log "already present: $DEST/usr/bin/qemu-aarch64"
        "$DEST/usr/bin/qemu-aarch64" --version | head -1
        exit 0
    fi
    log "existing binary is not runnable; re-extracting"
fi

command -v curl >/dev/null 2>&1 || die "curl is required"
command -v tar >/dev/null 2>&1 || die "tar is required"
command -v zstd >/dev/null 2>&1 || die "zstd is required (Arch: zstd)"

mkdir -p "$DEST"
tmp="$(mktemp "$DEST/qemu-user.XXXXXX.pkg.tar.zst")"
trap 'rm -f "$tmp"' EXIT

log "downloading $PKG_URL"
if ! curl -sSL --max-time 300 -o "$tmp" "$PKG_URL"; then
    die "download failed"
fi

have="$(sha256sum "$tmp" | cut -d' ' -f1)"
if [ "$have" != "$SHA" ]; then
    die "sha256 mismatch: expected $SHA, got $have"
fi
log "sha256 verified: $SHA"

tar --zstd -xf "$tmp" -C "$DEST"
rm -f "$tmp"
trap - EXIT

binary="$DEST/usr/bin/qemu-aarch64"
[ -x "$binary" ] || die "extraction did not produce $binary"
log "extracted: $binary"
"$binary" --version | head -1
log "no install, no root, no system path changed"
