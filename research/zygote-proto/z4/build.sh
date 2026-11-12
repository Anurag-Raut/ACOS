#!/usr/bin/env bash
# Z4: build the multi-session research zygote guest dex + fork/sandbox shim.
# Reuses Z3's source list; only the output directory differs so Z3's own
# reproduction command keeps working.
#   ZBUILD=z4-build research/zygote-proto/z3/build.sh
set -euo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ZBUILD="${ZBUILD:-z4-build}" exec "$HERE/../z3/build.sh"
