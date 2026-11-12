#!/usr/bin/env bash
# Z6 second batch (one lock acquisition): arm (c) at every N, then the
# release-mechanism runs ((c) and (a) 20 s probes, the (a)+heap-policy control).
set -uo pipefail
D=$(cd "$(dirname "$0")" && pwd)
"$D/run-z6.sh" c
"$D/run-z6-probe.sh"
