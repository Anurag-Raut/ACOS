#!/usr/bin/env bash
# Install ACOS into a self-contained prefix (REL1a).
#
#   tools/release/install.sh [--prefix DIR] [--version V] [--from-target DIR] [--force]
#                             [--register-mcp]
#
# The install root is a copy of the checkout's *runtime* trees under
# <prefix>/share/acos/<version>/ with the same relative layout the code already
# uses (`.cache/art/sysroot`, `.cache/boot/android-34`,
# `crates/acos-art/guest/java`), plus the three binaries. `<prefix>/bin/acos`
# and `<prefix>/bin/acos-mcp` are thin wrappers that export ACOS_HOME and exec
# the versioned binary, so ACOS runs from any directory with no checkout.
#
# Nothing here needs root, downloads anything, or touches the source tree.
set -euo pipefail

SELF_DIR=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
REPO=$(cd -- "$SELF_DIR/../.." && pwd)

PREFIX=${ACOS_PREFIX:-$HOME/.local}
VERSION=0.2.3
FROM_TARGET=
FORCE=0
REGISTER_MCP=0

die() {
    printf 'install.sh: %s\n' "$*" >&2
    exit 1
}

usage() {
    cat <<EOF
Usage: $(basename "$0") [options]

Builds (or takes) the ACOS release binaries, installs them plus their runtime
assets into <prefix>/share/acos/<version>/, points <prefix>/share/acos/current
at it, and writes <prefix>/bin/{acos,acos-mcp} wrappers that export ACOS_HOME.

Options:
  --prefix DIR       install prefix (default: \$ACOS_PREFIX, else ~/.local)
  --version V        version directory to install into (default: $VERSION)
  --from-target DIR  use DIR/release binaries instead of building
  --force            overwrite an existing version directory built from
                     something else
  --register-mcp     register the installed wrappers with Claude Code (user
                     scope) after installing; default off
  -h, --help         this text

Layout (under <prefix>/share/acos/<version>/):
  bin/acos bin/acos-art bin/acos-mcp
  .cache/art/sysroot              read-only ART sysroot (the framework)
  .cache/boot/android-34          fonts, overlays, bootclasspath
  crates/acos-art/guest/java      guest Java sources (compiled at run time)
  VERSION MANIFEST.txt            what this root contains

Mutable state (data, sessions, sockets, build caches, the shared boot image)
is created on demand under the same root: <prefix>/share/acos/<version>/.cache/.

Uninstall: rm -rf <prefix>/share/acos/<version> <prefix>/bin/acos <prefix>/bin/acos-mcp
(and <prefix>/share/acos if no other version is left).
EOF
}

while [ $# -gt 0 ]; do
    case "$1" in
        --prefix) PREFIX=$2; shift 2 ;;
        --prefix=*) PREFIX=${1#*=}; shift ;;
        --version) VERSION=$2; shift 2 ;;
        --version=*) VERSION=${1#*=}; shift ;;
        --from-target) FROM_TARGET=$2; shift 2 ;;
        --from-target=*) FROM_TARGET=${1#*=}; shift ;;
        --force) FORCE=1; shift ;;
        --register-mcp) REGISTER_MCP=1; shift ;;
        -h|--help) usage; exit 0 ;;
        *) die "unknown argument: $1 (try --help)" ;;
    esac
done

[ -n "$PREFIX" ] || die "--prefix must not be empty"
[ -n "$VERSION" ] || die "--version must not be empty"
case "$VERSION" in
    */*|.*) die "--version must be a plain directory name (got '$VERSION')" ;;
esac

SHARE="$PREFIX/share/acos"
DEST="$SHARE/$VERSION"
BINS=(acos acos-art acos-mcp)

# ---------------------------------------------------------------- binaries

if [ -n "$FROM_TARGET" ]; then
    BIN_DIR=$(cd -- "$FROM_TARGET" 2>/dev/null && pwd) ||
        die "--from-target $FROM_TARGET does not exist"
    BIN_DIR="$BIN_DIR/release"
    [ -d "$BIN_DIR" ] || die "--from-target $FROM_TARGET has no release/ directory"
    printf 'using binaries from %s\n' "$BIN_DIR"
else
    [ -f "$REPO/Cargo.toml" ] || die "no Cargo.toml at $REPO (run from a checkout)"
    command -v cargo >/dev/null || die "cargo is not on PATH"
    TARGET_DIR=${CARGO_TARGET_DIR:-$REPO/target}
    case "$TARGET_DIR" in
        /*) ;;
        *) TARGET_DIR="$REPO/$TARGET_DIR" ;;
    esac
    printf 'building release binaries (%s)\n' "$TARGET_DIR"
    ( cd "$REPO" && CARGO_TARGET_DIR="$TARGET_DIR" \
        cargo build --release --offline -p acos-cli -p acos-art -p acos-mcp )
    BIN_DIR="$TARGET_DIR/release"
fi
for bin in "${BINS[@]}"; do
    [ -x "$BIN_DIR/$bin" ] || die "missing $BIN_DIR/$bin"
done

# ------------------------------------------------------------ version dir

if [ -d "$DEST" ]; then
    if [ -f "$DEST/VERSION" ] && [ "$(cat "$DEST/VERSION")" = "$VERSION" ]; then
        printf 'updating existing %s in place (idempotent)\n' "$DEST"
    elif [ "$FORCE" -eq 1 ]; then
        printf 'overwriting %s (--force)\n' "$DEST"
        rm -rf "$DEST"
    else
        die "$DEST exists and was not installed for version $VERSION; pass --force to replace it"
    fi
fi
mkdir -p "$DEST/bin" "$PREFIX/bin"

# ---------------------------------------------------------------- assets
# The runtime trees are copied from their *resolved* location (a worktree's
# `.cache/art` is a symlink into the main checkout) while symlinks **inside**
# them are preserved: those are the sysroot's own relative links, and some of
# them are already dangling in the source extraction. Afterwards the tree is
# checked for a link that escapes the install root — an installed ACOS must not
# point back into a checkout.
copy_tree() {
    local rel=$1 required=$2
    local src
    src=$(readlink -f -- "$REPO/$rel" 2>/dev/null || true)
    if [ -z "$src" ] || [ ! -e "$src" ]; then
        if [ "$required" -eq 1 ]; then
            die "missing runtime asset $REPO/$rel (run from a checkout that has built it)"
        fi
        printf 'skip (absent) %s\n' "$rel"
        return 0
    fi
    mkdir -p "$DEST/$(dirname -- "$rel")"
    printf 'copy %s (%s)\n' "$rel" "$src"
    rm -rf "$DEST/$rel"
    cp -a --reflink=auto -- "$src" "$DEST/$rel"
}

copy_tree ".cache/art/sysroot" 1
copy_tree ".cache/boot/android-34" 1
copy_tree "crates/acos-art/guest/java" 1
copy_tree "research/art-feasibility/a-dalvikvm/bootclasspath.txt" 0

# Fail closed if the copy still points outside the install root.
escaped=$(find "$DEST" -type l 2>/dev/null | while read -r link; do
    target=$(readlink -f -- "$link" 2>/dev/null || true)
    [ -n "$target" ] || continue # already dangling in the source tree
    case "$target" in
        "$DEST"/*) ;;
        *) printf '%s -> %s\n' "$link" "$target" ;;
    esac
done)
[ -z "$escaped" ] || die "the copied tree has symlinks outside $DEST:
$escaped"

for bin in "${BINS[@]}"; do
    install -m 0755 "$BIN_DIR/$bin" "$DEST/bin/$bin"
done

# ------------------------------------------------------------- metadata

printf '%s\n' "$VERSION" >"$DEST/VERSION"
{
    printf 'ACOS %s\n' "$VERSION"
    printf 'installed %s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)"
    printf 'source %s\n' "${FROM_TARGET:-$BIN_DIR}"
    printf 'assets:\n'
    ( cd "$DEST" && find bin .cache/art/sysroot .cache/boot/android-34 \
        crates/acos-art/guest/java -type f -print 2>/dev/null | LC_ALL=C sort )
} >"$DEST/MANIFEST.txt"

# -------------------------------------------------------------- wrappers
# ACOS_HOME names the *versioned* root, not `current`, so an install that
# lands while a session runs cannot swap the tree underneath it.
write_wrapper() {
    local name=$1 target="$DEST/bin/$1"
    local path="$PREFIX/bin/$name"
    {
        printf '#!/bin/sh\n'
        printf '# ACOS %s launcher (generated by tools/release/install.sh).\n' "$VERSION"
        printf '# ACOS_HOME defaults to this install; an explicit one wins.\n'
        printf 'ACOS_HOME="${ACOS_HOME:-%s}"\n' "$DEST"
        printf 'export ACOS_HOME\n'
        printf 'exec "%s" "$@"\n' "$target"
    } >"$path"
    chmod 0755 "$path"
    printf 'wrote %s\n' "$path"
}
write_wrapper acos
write_wrapper acos-mcp

ln -sfn "$VERSION" "$SHARE/current"

# -------------------------------------------------------------- next steps
if [ "$REGISTER_MCP" -eq 1 ]; then
    printf '\nregistering the MCP server (user scope)\n'
    "$SELF_DIR/register-mcp.sh" --prefix "$PREFIX"
fi

# ------------------------------------------------------------------ report
SIZE=$(du -sh "$DEST" | cut -f1)
FILES=$(grep -c '^bin/\|^\.cache/\|^crates/' "$DEST/MANIFEST.txt" || true)
cat <<EOF

installed ACOS $VERSION
  root    $DEST
  size    $SIZE ($FILES files listed in MANIFEST.txt)
  current $SHARE/current -> $VERSION
  wrapper $PREFIX/bin/acos, $PREFIX/bin/acos-mcp

Next steps
  1. preflight:   $PREFIX/bin/acos doctor
  2. try it:      $PREFIX/bin/acos inspect app.apk
  3. run:         $PREFIX/bin/acos launch app.apk --engine art --json
  4. MCP (Claude Code, user scope):
       $PREFIX/bin/acos-mcp is registered by:
       tools/release/register-mcp.sh --prefix $PREFIX
     (or re-run this installer with --register-mcp; --dry-run prints the command)
EOF
