#!/usr/bin/env bash
# Z5 follow-up under ONE lock acquisition, the remaining two configurations:
#   * arm (c) at N=20 — the only zygote N that the earlier acquisition did not reach;
#   * arm (a) at N=1/5/10/20 as a fully warm repeat (`-r2`): arm (a)'s first run paid a
#     one-off dex build (see Z5-gate.md §4), which inflated only its ready time and CPU.
set -uo pipefail
D=$(cd "$(dirname "$0")" && pwd)
Z5_NS="20" "$D/run-z5.sh" c
Z5_NS="1 5 10 20" Z5_SUFFIX="-r2" "$D/run-z5.sh" a
