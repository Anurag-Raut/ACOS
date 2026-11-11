#!/usr/bin/env bash
# public-libs.sh -- grant the bionic classloader namespace (clns-1) access to the
# two system libraries the graphics seam needs.
#
# WHY: a library loaded from /data lives in namespace "clns-1"
# (permitted_paths=/data).  libnativeloader only links clns-1 -> "default" for
# libraries listed in /system/etc/public.libraries.txt, so dlopen()ing
# /system/lib64/libhwui.so or libandroid_runtime.so fails with
#   "... is not accessible for the namespace: [name="clns-1", permitted_paths="/data"]"
# (measured; see results/art-feasibility/E2E.md §4 blocker B1).
#
# This edits the *scratch* sysroot from E1A only (no ACOS source, no sudo).
# Undo: scripts/public-libs.sh --restore
set -euo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ACOS_ROOT="$(cd "$HERE/../../../.." && pwd)"
F="$ACOS_ROOT/.cache/art-exp/sysroot/system/etc/public.libraries.txt"
BAK="$ACOS_ROOT/.cache/art-exp/sysroot/system/etc/public.libraries.txt.e2-orig"
if [ "${1:-}" = "--restore" ]; then
    [ -f "$BAK" ] && cp "$BAK" "$F" && echo "restored $F"
    exit 0
fi
[ -f "$F" ] || { echo "no sysroot at $F (run E1A extract scripts first)" >&2; exit 1; }
[ -f "$BAK" ] || cp "$F" "$BAK"
for lib in libhwui.so libandroid_runtime.so; do
    grep -qxF "$lib" "$F" || echo "$lib" >> "$F"
done
tail -2 "$F"
