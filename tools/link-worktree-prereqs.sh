#!/usr/bin/env bash
#
# link-worktree-prereqs.sh -- make the large test prerequisites visible inside
# an agent worktree (HYG4). Called by tools/agent-env.sh; safe to run by hand.
#
#   tools/link-worktree-prereqs.sh
#
# A worktree's `.cache` is its own directory, but the heavy prerequisites live
# once in the shared cache (the main checkout's `.cache`). This makes a symlink
# in `$WT/.cache` for each of them when it is missing:
#
#   boot/android-34  art  gaten  qemu-user  bionic-arm64  flutter-1  corpus
#
# and builds the seccomp guard (`.cache/arm64-guard/libacos_seccomp_guard.so`)
# with tools/build-arm64-guard.sh when it is missing. The guard is built per
# worktree because it is not in the shared cache.
#
# Idempotent: an existing, correct link or guard is left alone. A shared
# prerequisite that does not exist is reported, not created (the test that
# needs it then fails with its own remedy, see acos_test_support::require_prereq).
#
# Env overrides:
#   ACOS_SHARED_CACHE   shared cache to link from (default: the main checkout)
#   ACOS_NO_GUARD_BUILD=1  do not build the guard here (report only)
set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SHARED="${ACOS_SHARED_CACHE:-/home/Qui/Projects/acos/.cache}"
mkdir -p "$ROOT/.cache" || exit 1

status=0
for name in boot/android-34 art gaten qemu-user bionic-arm64 flutter-1 corpus; do
  src="$SHARED/$name"
  dst="$ROOT/.cache/$name"
  if [ -L "$dst" ] && [ -e "$dst" ]; then
    continue
  fi
  if [ -e "$dst" ]; then
    continue # a real directory the worktree owns; never replace it
  fi
  if [ -e "$src" ]; then
    mkdir -p "$(dirname "$dst")"
    ln -sfn "$src" "$dst"
  else
    echo "link-worktree-prereqs: shared $src is missing (tests needing it will fail with their remedy)" >&2
    status=1
  fi
done

guard="$ROOT/.cache/arm64-guard/libacos_seccomp_guard.so"
if [ ! -s "$guard" ]; then
  if [ "${ACOS_NO_GUARD_BUILD:-0}" = 1 ]; then
    echo "link-worktree-prereqs: $guard is missing (ACOS_NO_GUARD_BUILD=1, not built)" >&2
    status=1
  elif [ -x "$ROOT/tools/build-arm64-guard.sh" ]; then
    echo "link-worktree-prereqs: building $guard (tools/build-arm64-guard.sh)" >&2
    "$ROOT/tools/build-arm64-guard.sh" >&2 || { echo "link-worktree-prereqs: guard build FAILED" >&2; status=1; }
  fi
fi
exit "$status"
