#!/usr/bin/env bash
# Z6 control for the release question (§5): "would the exec'd arm (a) release the
# same memory if ART ran the same trim?"  No product change is possible here (a
# forced System.gc() hook would be a product edit), so the trim is *induced* with
# the existing W48 heap seam: a managed-heap growth limit low enough that ART must
# collect and shrink the exec'd guest's own heap.  (d) at N=1 is representative:
# one exec'd guest costs the same at every N.
#
#   1. 24 MiB growth limit, 90 % target utilisation — can the exec path live in
#      24 MiB, and if it can, how big is its region space then?
#   2. 16 MiB — the same question one step tighter than the live set we expect.
#   3. 24 MiB at N=5 with the 20 s 1 Hz probe — does a *drop* happen on the exec
#      path under heap pressure, in which mapping, and when?
set -uo pipefail
D=$(cd "$(dirname "$0")" && pwd)
CACHE=/home/Qui/Projects/acos/.cache
export ACOS_ART_HEAPTARGETUTILIZATION=${Z6_HEAPTARGETUTILIZATION:-0.9}
export Z6_SAMPLES=${Z6_SAMPLES:-0.5,60}

Z6_NS=1 Z6_SUFFIX=-h24 Z6_HEAPGROWTHLIMIT=24m "$D/run-z6.sh" d
Z6_NS=1 Z6_SUFFIX=-h16 Z6_HEAPGROWTHLIMIT=16m "$D/run-z6.sh" d
Z6_NS=5 Z6_SUFFIX=-h24probe Z6_HEAPGROWTHLIMIT=24m Z6_PROBE=20 "$D/run-z6.sh" d
