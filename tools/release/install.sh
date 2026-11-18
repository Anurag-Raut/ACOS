#!/usr/bin/env bash
# Install ACOS into a self-contained prefix (REL1a).
#
#   tools/release/install.sh [--prefix DIR] [--version V] [--from-target DIR] [--force]
#                             [--register-mcp]
#
# The install root is a copy of the checkout's *runtime* trees under
# <prefix>/share/acos/<version>/ with the same relative layout the code already
# uses (`.cache/art/sysroot`, `.cache/boot/android-34`,
# `crates/acos-art/guest/java`), plus the four binaries and — when a pinned one
# is available — the `network` module's `pasta` transport at `libexec/pasta`.
# `<prefix>/bin/acos`
# and `<prefix>/bin/acos-mcp` are thin wrappers that export ACOS_HOME and exec
# the versioned binary, so ACOS runs from any directory with no checkout.
#
# Nothing here needs root, downloads anything, or touches the source tree.
set -euo pipefail

SELF_DIR=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
REPO=$(cd -- "$SELF_DIR/../.." && pwd)

PREFIX=${ACOS_PREFIX:-$HOME/.local}
# The default version is the workspace version (Cargo.toml), so the two cannot drift.
VERSION=$(awk -F'"' '/^\[workspace\.package\]/{w=1} w && /^version *=/{print $2; exit}' "$REPO/Cargo.toml")
FROM_TARGET=
FORCE=0
REGISTER_MCP=0
PASTA=

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
  --pasta PATH       install this `pasta` binary as the relay's transport
                     (default: $ACOS_NET_PASTA, then <repo>/libexec/pasta, then
                     `pasta` on PATH); without one the network module stays
                     unavailable and the installer prints how to build it
  --force            overwrite an existing version directory built from
                     something else
  --register-mcp     register the installed wrappers with Claude Code (user
                     scope) after installing; default off
  -h, --help         this text

Layout (under <prefix>/share/acos/<version>/):
  bin/acos bin/acos-art bin/acos-mcp bin/acos-netd
  libexec/pasta                   the pinned static transport (ADR-0030)
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
        --pasta) PASTA=$2; shift 2 ;;
        --pasta=*) PASTA=${1#*=}; shift ;;
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
# acos-netd is the network module's relay supervisor (ADR-0030); a client finds
# it next to its own executable, so an install without it could never attach
# `network:real` however good its pasta is.
BINS=(acos acos-art acos-mcp acos-netd)

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
        cargo build --release --offline -p acos-cli -p acos-art -p acos-mcp -p acos-netd )
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

# ------------------------------------------------- the WebView provider APK
# The `webview` module (ADR-0031) installs `com.android.webview` as a system app,
# so the installed tree must *carry* that APK: an installed ACOS has no checkout
# to build it from, and "run build-apps.sh first" is exactly the separate step a
# fresh install must not need. The image build
# (`tools/extract-bootclasspath.sh`) ships it under `system-apps/`, the copy
# above carries it, and this block is the belt-and-braces: if the source image
# predates that (or the copy lost it), build it into the install root with the
# one build implementation.
PROVIDER_REL=".cache/boot/android-34/system-apps/com.android.webview.apk"
ensure_webview_provider() {
    if [ -f "$DEST/$PROVIDER_REL" ]; then
        printf 'webview provider: %s (%s bytes)\n' "$PROVIDER_REL" \
            "$(stat -c '%s' "$DEST/$PROVIDER_REL")"
        return 0
    fi
    printf 'webview provider: missing from the image, building it into the install\n'
    [ -x "$REPO/tools/build-webview-provider.sh" ] ||
        die "the image has no $PROVIDER_REL and $REPO/tools/build-webview-provider.sh is missing"
    mkdir -p "$DEST/$(dirname -- "$PROVIDER_REL")"
    "$REPO/tools/build-webview-provider.sh" --out "$DEST/$PROVIDER_REL" ||
        die "building the WebView provider into $DEST failed"
    [ -f "$DEST/$PROVIDER_REL" ] ||
        die "the WebView provider build produced no APK at $DEST/$PROVIDER_REL"
    printf 'webview provider: %s (%s bytes, built at install)\n' "$PROVIDER_REL" \
        "$(stat -c '%s' "$DEST/$PROVIDER_REL")"
}
ensure_webview_provider

# ------------------------------------------------- ADR-0030's transport (pasta)
# The `network` module's `real` profile runs the session's traffic through one
# static `pasta` per host. It is *pinned*: `research/spike-net/artifacts/
# passt-build.txt` records the source tag, the build command, the licence and the
# binary's sha256, and the relay reports the tag it found next to the expected
# one.
#
# The install must be self-contained (an installed ACOS has no checkout to find
# a binary in), so this block copies the pinned binary to the one location
# `acos doctor`, the relay supervisor and the scenario runners all read:
# `<install root>/libexec/pasta` (see crates/acos-netd-ctl/src/pasta.rs).
# Nothing is downloaded: with no pinned binary on the host the installer prints
# the exact recipe and the install stays honest (`pasta: missing`, and
# `needs_env: [pasta]` scenarios are skipped rather than faked).
PASTA_REL="libexec/pasta"
PASTA_RECORD="$REPO/research/spike-net/artifacts/passt-build.txt"
PASTA_TAG=$(awk '/^pasta /{print $2; exit}' "$PASTA_RECORD" 2>/dev/null || true)
PASTA_SHA=$(awk '/^sha256:/{print $2; exit}' "$PASTA_RECORD" 2>/dev/null || true)
PASTA_TAG=${PASTA_TAG:-2026_10_02.cba3570}

pasta_recipe() {
    cat <<EOF
  build it (pinned tag $PASTA_TAG, sha256 $PASTA_SHA):
      git clone --depth 1 -b $PASTA_TAG https://passt.top/passt pasta-src
      make -C pasta-src static VERSION=$PASTA_TAG
      $0 --prefix $PREFIX --pasta pasta-src/pasta
  ...or drop it at <repo>/libexec/pasta and re-run this installer.
  \`acos doctor\` reports \`pasta: missing\` until then, and scenarios that
  declare \`needs_env: [pasta]\` are skipped (the default network profile is
  off, so nothing else is affected).
EOF
}

pasta_source() {
    if [ -n "$PASTA" ]; then printf '%s\n' "$PASTA"; return 0; fi
    if [ -n "${ACOS_NET_PASTA:-}" ]; then printf '%s\n' "$ACOS_NET_PASTA"; return 0; fi
    local cand
    for cand in "$REPO/libexec/pasta" "$REPO/.cache/spike-net/bin/pasta"; do
        [ -f "$cand" ] && { printf '%s\n' "$cand"; return 0; }
    done
    if command -v pasta >/dev/null 2>&1; then command -v pasta; return 0; fi
    return 1
}

install_pasta() {
    local src sha version
    if ! src=$(pasta_source); then
        printf 'pasta: not found on this host; the network module stays unavailable\n'
        pasta_recipe
        return 0
    fi
    [ -f "$src" ] || { printf 'pasta: %s is not a file; skipping\n' "$src"; pasta_recipe; return 0; }
    sha=$(sha256sum -- "$src" | cut -d' ' -f1)
    version=$("$src" --version 2>/dev/null | head -1 | awk '{print $NF}') || version=
    if [ -n "$PASTA_SHA" ] && [ "$sha" != "$PASTA_SHA" ]; then
        printf 'pasta: %s is %s (version %s), not the pinned build (%s);\n' \
            "$src" "$sha" "${version:-unknown}" "$PASTA_SHA"
        printf 'pasta: NOT installing it — the relay would run an unmeasured transport.\n'
        pasta_recipe
        return 0
    fi
    if [ -n "$PASTA_TAG" ] && [ -n "$version" ] && [ "$version" != "$PASTA_TAG" ]; then
        printf 'pasta: %s reports version %s, expected the pinned tag %s; skipping\n' \
            "$src" "$version" "$PASTA_TAG"
        pasta_recipe
        return 0
    fi
    mkdir -p "$DEST/$(dirname -- "$PASTA_REL")"
    install -m 0755 -- "$src" "$DEST/$PASTA_REL"
    printf 'pasta: %s (%s bytes, version %s, sha256 %s) -> %s\n' \
        "$src" "$(stat -c '%s' "$src")" "${version:-unknown}" "$sha" "$PASTA_REL"
}
install_pasta
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

# --------------------------------------------------- software GL (GLSHIP)
# The guest's software GL provider (ANGLE on SwiftShader) is part of the sysroot
# the copy above already carried (`<root>/.cache/art/sysroot/gles/<provider>/`),
# so an install is GL-capable with no extra step. Report it, and fail closed if
# the manifest it ships is unreadable: `acos doctor` verifies the bytes on every
# launch, so a half-copied provider must not look installed.
GLES_LINE="none"
GLES_DIR="$DEST/.cache/art/sysroot/gles/angle-swiftshader"
if [ -f "$GLES_DIR/manifest.json" ]; then
    GLES_LINE=$(python3 - "$GLES_DIR/manifest.json" <<'GLESMANIFEST'
import json, sys
m = json.load(open(sys.argv[1]))
total = sum(f["size"] for f in m["files"])
print(f'{m["provider"]} | {m["version"]} | {m["licence"]} | {total // (1024 * 1024)} MiB staged')
GLESMANIFEST
    ) || die "$GLES_DIR/manifest.json is not a provider manifest (the sysroot copy is broken)"
else
    printf 'gles: no provider in the sysroot (software GL off; apps run exactly as before)\n'
fi

# ------------------------------------------------------------- metadata

printf '%s\n' "$VERSION" >"$DEST/VERSION"
{
    printf 'ACOS %s\n' "$VERSION"
    printf 'installed %s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)"
    printf 'source %s\n' "${FROM_TARGET:-$BIN_DIR}"
    printf 'gles %s\n' "$GLES_LINE"
    printf 'assets:\n'
    ( cd "$DEST" && find bin libexec .cache/art/sysroot .cache/boot/android-34 \
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
FILES=$(grep -c '^bin/\|^libexec/\|^\.cache/\|^crates/' "$DEST/MANIFEST.txt" || true)
cat <<EOF

installed ACOS $VERSION
  root    $DEST
  size    $SIZE ($FILES files listed in MANIFEST.txt)
  current $SHARE/current -> $VERSION
  wrapper $PREFIX/bin/acos, $PREFIX/bin/acos-mcp
  gles    $GLES_LINE

Next steps
  1. preflight:   $PREFIX/bin/acos doctor   (the network line reads "pasta: <version>" or missing)
  2. try it:      $PREFIX/bin/acos inspect app.apk
  3. run:         $PREFIX/bin/acos launch app.apk --engine art --json
  4. real network (optional): the network module's real profile needs the
     pinned pasta; this installer put it at $DEST/libexec/pasta when one was
     available (see docs/INSTALL.md, "The network module's pasta")
  5. MCP (Claude Code, user scope):
       $PREFIX/bin/acos-mcp is registered by:
       tools/release/register-mcp.sh --prefix $PREFIX
     (or re-run this installer with --register-mcp; --dry-run prints the command)
EOF
