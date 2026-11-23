#!/usr/bin/env bash
#
# agent-env.sh -- the one place an agent worktree gets its build and test
# environment.
#
#   source tools/dev/agent-env.sh
#
# BRIEF_COMMON tells every agent to create `.cache/env.sh` in its worktree
# (because that path is stable while worktree names are not) and to source it
# before building. Make that file a one-liner so there is exactly one
# definition to maintain:
#
#   mkdir -p .cache && echo 'source tools/dev/agent-env.sh' > .cache/env.sh
#
# What it sets, and why:
#
#   * the ART/fixture test env the briefs require -- ACOS_FONT_DIR, the
#     .cache/{boot/android-34,art,gaten} symlinks into the shared caches and
#     ACOS_VIEW_TEST_DIR (unix socket paths must fit SUN_LEN). It never sets
#     ACOS_BOOT_DIR or XDG_RUNTIME_DIR, and it does not touch ACOS_HOME.
#   * the small-target build flags: CARGO_PROFILE_{DEV,TEST}_DEBUG=0 and
#     CARGO_INCREMENTAL=0 (a debug build is several GB otherwise; incremental
#     output cannot be cached, see below).
#   * the shared compiler cache: RUSTC_WRAPPER=sccache, SCCACHE_DIR,
#     SCCACHE_CACHE_SIZE and SCCACHE_BASEDIRS. Every agent worktree on this host
#     then reuses the crates every other worktree already compiled instead of
#     rebuilding the whole workspace. See docs/DEV_WORKFLOW.md ("Shared compiler
#     cache") and results/handoff/BUILDCACHE/README.md for the measurements.
#
# It deliberately does NOT set CARGO_TARGET_DIR: each track keeps its own target
# directory (the brief's rule), and the cache is what makes that cheap.
#
# Idempotent and safe to source from a script (`set -u` friendly).

# --- the worktree this script lives in -------------------------------------
ACOS_WT="$(cd "$(dirname "${BASH_SOURCE[0]:-$0}")/../.." && pwd)"
export ACOS_WT

# --- ART / fixture test env (BRIEF_COMMON) ---------------------------------
export ACOS_FONT_DIR=/home/Qui/Projects/acos/.cache/boot/android-34/fonts
export ACOS_VIEW_TEST_DIR="$ACOS_WT/.cache/lv"
mkdir -p "$ACOS_WT/.cache/lv"

# The shared prerequisites (boot, art, gaten, qemu-user, bionic-arm64, flutter-1,
# corpus) are linked from the main checkout, and the arm64 seccomp guard is built
# once per worktree, by tools/dev/link-worktree-prereqs.sh (HYG4). Idempotent.
bash "$ACOS_WT/tools/dev/link-worktree-prereqs.sh" || \
    echo "acos-env: link-worktree-prereqs reported a missing prerequisite (see above)" >&2

# --- small target dirs ------------------------------------------------------
export CARGO_PROFILE_DEV_DEBUG=0
export CARGO_PROFILE_TEST_DEBUG=0
export CARGO_PROFILE_RELEASE_DEBUG=0
# sccache cannot cache incremental compiles at all (`cannot_cache!("incremental")`),
# so incremental would silently disable the cache and grow the target dir.
export CARGO_INCREMENTAL=0

# --- shared compiler cache --------------------------------------------------
# The wrapper is installed by tools/sccache/build.sh (user-local, no sudo):
#   ~/.cargo/bin/sccache -- stock sccache v0.19.0 plus one patch, see
#   tools/sccache/README.md.
_acos_sccache="${ACOS_SCCACHE:-}"
if [ -z "$_acos_sccache" ]; then
    if command -v sccache >/dev/null 2>&1; then
        _acos_sccache="$(command -v sccache)"
    elif [ -x "$HOME/.cargo/bin/sccache" ]; then
        _acos_sccache="$HOME/.cargo/bin/sccache"
    fi
fi
if [ -n "$_acos_sccache" ]; then
    # so `sccache --show-stats` / `--zero-stats` work in an agent shell
    case ":$PATH:" in
        *":$(dirname "$_acos_sccache"):"*) ;;
        *) export PATH="$(dirname "$_acos_sccache"):$PATH" ;;
    esac
    export RUSTC_WRAPPER="$_acos_sccache"
    # One cache for the whole host. 40 GiB is ~2x a full workspace build and is
    # evicted LRU by sccache itself.
    export SCCACHE_DIR="${SCCACHE_DIR:-/home/Qui/.cache/sccache}"
    export SCCACHE_CACHE_SIZE="${SCCACHE_CACHE_SIZE:-40G}"
    # These roots are interchangeable checkouts of this repository: the same
    # sources under any of them hash to one cache key. The list is part of the
    # key, so it must be identical for every agent on the host (it is: this
    # file is the only place that sets it).
    # (the three layouts worktrees are actually checked out in: the t3 tree, the
    # main checkout, and the older `Projects/acos/.cache/wt-*` worktrees)
    export SCCACHE_BASEDIRS="${SCCACHE_BASEDIRS:-/home/Qui/.t3/worktrees/acos:/home/Qui/Projects/acos/.cache:/home/Qui/Projects/acos}"
    # The daemon keeps the configuration it was started with: after changing any
    # SCCACHE_* value below, run `sccache --stop-server` once so the next build
    # starts a server with the new settings (otherwise the old basedirs keep
    # being used and cross-worktree hits silently stop).
    # Keep the server up between builds (default is 10 min); it is a few MB.
    export SCCACHE_IDLE_TIMEOUT="${SCCACHE_IDLE_TIMEOUT:-1800}"
    ACOS_SCCACHE_STATUS="sccache=$_acos_sccache cache=$SCCACHE_DIR ($SCCACHE_CACHE_SIZE)"
else
    unset RUSTC_WRAPPER
    ACOS_SCCACHE_STATUS="sccache NOT installed: builds work but are uncached (run tools/sccache/build.sh)"
fi
unset _acos_sccache

if [ "${ACOS_ENV_QUIET:-0}" != 1 ]; then
    printf 'acos-env: %s\n  font=%s view-test-dir=%s debug=0 incremental=0\n  %s\n' \
        "$ACOS_WT" "$ACOS_FONT_DIR" "$ACOS_VIEW_TEST_DIR" "$ACOS_SCCACHE_STATUS"
fi
