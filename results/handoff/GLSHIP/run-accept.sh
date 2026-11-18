#!/usr/bin/env bash
# GLSHIP acceptance in one lock acquisition: the default-flag gates, then the
# 5-rep default-on vs off cost A/B.
#   tools/art-runtime/run-exclusive.sh --task GLSHIP -- \
#       tools/guarded.sh --mem 14G --time 1500 -- results/handoff/GLSHIP/run-accept.sh
set -uo pipefail
echo "=== gates (default provider, no flag) ==="
results/handoff/GLSHIP/run-gates.sh
echo "=== a11y-screenshot-wrap with the provider explicitly off ==="
results/handoff/GLSHIP/run-arm.sh gate-a11y-wrap-off scenarios/a11y-screenshot-wrap.yaml ACOS_GUEST_GLES=off
echo "=== cost A/B (5 reps, interleaved) ==="
results/handoff/GLSHIP/run-ab.sh
