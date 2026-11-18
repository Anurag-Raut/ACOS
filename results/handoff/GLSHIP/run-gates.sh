#!/usr/bin/env bash
# GLSHIP acceptance gates: every scenario with the **default** flag (no
# ACOS_GUEST_GLES in the environment), so what passes is what an agent gets on
# a stock install. One lock acquisition:
#   tools/art-runtime/run-exclusive.sh --task GLSHIP -- \
#       tools/guarded.sh --mem 14G --time 900 -- results/handoff/GLSHIP/run-gates.sh
set -uo pipefail
R=results/handoff/GLSHIP/run-arm.sh
$R gate-counter        scenarios/counter.yaml
$R gate-form           scenarios/form-validation.yaml
$R gate-a11y-probe     scenarios/a11y-probe.yaml
$R gate-a11y-wrap      scenarios/a11y-screenshot-wrap.yaml
$R gate-webview-probe  scenarios/webview-probe.yaml
# gles-software: 3 concurrent sessions, default provider (no flag)
for i in 1 2 3; do
  $R gate-gles-$i scenarios/gles-software.yaml &
done
wait
