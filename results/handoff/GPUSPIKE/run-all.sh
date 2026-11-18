#!/usr/bin/env bash
# GPUSPIKE arms (one lock acquisition; run from the worktree root):
#   tools/art-runtime/run-exclusive.sh --task GPUSPIKE -- tools/guarded.sh --mem 14G --time 580 -- results/handoff/GPUSPIKE/run-all.sh
set -uo pipefail
R=results/handoff/GPUSPIKE/run-arm.sh
G=ACOS_GUEST_GLES=mesa-host
$R mesa-gles scenarios/gles-mesa-host.yaml $G
$R mesa-probe3 scenarios/egl-probe.yaml $G
