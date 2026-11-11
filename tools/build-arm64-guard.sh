#!/usr/bin/env bash
#
# build-arm64-guard.sh -- build libacos_seccomp_guard.so (the deferred seccomp
# LD_PRELOAD guard used by the arm64 qemu transport).
#
# The guard is a normal cargo cdylib (`crates/acos-seccomp-guard`), so
# `cargo build --workspace` also produces it. This script is a convenience for
# placing a copy where `acos-arm64` looks by default:
#
#     <repo>/.cache/arm64-guard/libacos_seccomp_guard.so
#
# The filter is not re-implemented here: the guard calls the audited
# `acos_sandbox::install_deferred_seccomp`.
#
# Usage:
#   tools/build-arm64-guard.sh
#
# Env overrides:
#   CARGO_TARGET_DIR   cargo target dir (default <repo>/.cache/target)
#   ACOS_GUARD_DIR     install dir (default <repo>/.cache/arm64-guard)
set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$ROOT/.cache/target}"
export CARGO_PROFILE_DEV_DEBUG="${CARGO_PROFILE_DEV_DEBUG:-0}"
export CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-2}"
DEST="${ACOS_GUARD_DIR:-$ROOT/.cache/arm64-guard}"

log() { printf '[arm64-guard] %s\n' "$*"; }

if [ -x "$ROOT/tools/guarded.sh" ]; then
    "$ROOT/tools/guarded.sh" --mem 8G --time 1800 -- \
        cargo build --manifest-path "$ROOT/Cargo.toml" -p acos-seccomp-guard
else
    cargo build --manifest-path "$ROOT/Cargo.toml" -p acos-seccomp-guard
fi

src="$CARGO_TARGET_DIR/debug/libacos_seccomp_guard.so"
[ -s "$src" ] || { log "ERROR: $src was not produced"; exit 1; }

mkdir -p "$DEST"
cp -f "$src" "$DEST/libacos_seccomp_guard.so"
log "installed $DEST/libacos_seccomp_guard.so"
sha256sum "$DEST/libacos_seccomp_guard.so"
log "use it via SeccompMode::DeferredPreload, or set ACOS_SECCOMP_GUARD to this path"
