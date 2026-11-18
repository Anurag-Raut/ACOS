#!/usr/bin/env bash
#
# build.sh -- build and install the ACOS shared compiler cache (sccache).
#
# Every agent worktree shares one sccache cache (see tools/agent-env.sh), which
# needs one patch on top of stock sccache: stock sccache hashes the compile cwd
# and the `CARGO_*` environment verbatim, so two checkouts of the same sources
# never share an entry no matter what `SCCACHE_BASEDIRS` says. The patch is
# `sccache-basedirs-cwd.patch` next to this script; see its header and
# tools/sccache/README.md for what it changes and how it was verified.
#
# usage:
#   tools/sccache/build.sh                 # build + install into ~/.cargo/bin
#   SCCACHE_PREFIX=/some/dir tools/sccache/build.sh   # install elsewhere
#
# The build is offline-friendly only after the first run (crates.io deps); it
# writes to $TMPDIR-style scratch under the *cache* directory, never /tmp, and
# removes the scratch when done (KEEP_SRC=1 keeps it).
set -euo pipefail

VERSION=v0.19.0
TARBALL="sccache-$VERSION-x86_64-unknown-linux-musl.tar.gz"
# sha256 of the release tarball this tree was built from (recorded here so a
# download is verifiable without trusting the release page).
TARBALL_SHA256=aa73c7270c997894038c0d8f0d297865f57569e1687883322bc431713d4bb9dd
# sha256 of the installed binary this script produced on 2026-11-18.
BINARY_SHA256=64a1b0e9327d99642a3a29ecfc14522448fff71d33b165a36ad7a4263b28b45a

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
SRC_DIR="${ACOS_SCCACHE_SRC:-$HOME/.cache/sccache-build-src}"
TARGET_DIR="${ACOS_SCCACHE_TARGET:-$HOME/.cache/sccache-build-target}"
PREFIX="${SCCACHE_PREFIX:-$HOME/.cargo/bin}"
PATCH="$ROOT/tools/sccache/sccache-basedirs-cwd.patch"

say() { printf 'sccache-build: %s\n' "$*"; }

command -v cargo >/dev/null || { say "cargo not found"; exit 1; }
command -v patch >/dev/null || { say "patch not found"; exit 1; }

mkdir -p "$SRC_DIR"
cd "$SRC_DIR"
if [ ! -d "sccache-$VERSION" ]; then
    if [ ! -f "$TARBALL" ]; then
        say "downloading $TARBALL"
        curl -fsSL --max-time 300 -O \
            "https://github.com/mozilla/sccache/releases/download/$VERSION/$TARBALL"
    fi
    got=$(sha256sum "$TARBALL" | cut -d' ' -f1)
    [ "$got" = "$TARBALL_SHA256" ] || { say "sha256 mismatch: $got"; exit 1; }
    say "sha256 ok ($got)"
    tar xzf "$TARBALL"
fi

cd "sccache-$VERSION"
if ! grep -q normalized_basedir_value src/compiler/rust.rs; then
    say "applying $PATCH"
    patch -p1 --forward < "$PATCH"
fi

# --no-default-features drops the remote-storage backends (S3/GCS/Azure/...,
# i.e. opendal). ACOS uses the local disk cache only, and those backends need a
# newer rustc than the host toolchain.
say "building (release, no default features)"
env CARGO_TARGET_DIR="$TARGET_DIR" \
    CARGO_INCREMENTAL=0 \
    CARGO_PROFILE_RELEASE_LTO=false \
    CARGO_PROFILE_RELEASE_CODEGEN_UNITS=16 \
    CARGO_PROFILE_RELEASE_DEBUG=0 \
    cargo build --release --locked --no-default-features

mkdir -p "$PREFIX"
install -m 755 "$TARGET_DIR/release/sccache" "$PREFIX/sccache"
installed=$(sha256sum "$PREFIX/sccache" | cut -d' ' -f1)
say "installed $PREFIX/sccache sha256=$installed"
[ "$installed" = "$BINARY_SHA256" ] || \
    say "note: binary differs from the recorded one (expected if the toolchain changed)"
"$PREFIX/sccache" --version

if [ "${KEEP_SRC:-0}" != 1 ]; then
    rm -rf "$TARGET_DIR"
    say "removed $TARGET_DIR (KEEP_SRC=1 keeps it)"
fi
