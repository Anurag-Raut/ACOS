#!/usr/bin/env bash
# artrun.sh -- the *no-namespace* launch path (env vars only, no bwrap).
#
# Recorded because it is the route the task suggested first ("if unprivileged
# userns is disabled, find another route, e.g. patching search paths via env
# only").  It gets real ART as far as printing `ART version 2.1.0 x86_64`, but
# without namespaces every boot-classpath jar is "not in a known location"
# (ART matches them against /apex/<module>/javalib/... and needs the bionic
# linker's per-APEX namespaces, which only exist via /linkerconfig/ld.config.txt
# at absolute paths, i.e. a mount namespace) and it aborts.  See
# results/art-feasibility/E1A.md §3 items 4-6 and §5.
#
# Use scripts/artroot.sh instead.
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
. "$HERE/env.sh"

export LD_LIBRARY_PATH="$ART_LD_LIBRARY_PATH"
[ -n "${ART_NO_LOGSHIM:-}" ] || export LD_PRELOAD="$ACOS_ROOT/.cache/art-exp/shim/liblogshim.so"
exec "$LINKER64" "$DALVIKVM" "$@"
