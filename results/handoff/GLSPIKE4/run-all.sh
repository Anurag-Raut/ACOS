#!/usr/bin/env bash
# GLSPIKE4 acceptance + cost arms (one lock acquisition; run from the worktree root):
#   tools/art-runtime/run-exclusive.sh --task GLSPIKE4 -- tools/guarded.sh --mem 14G --time 580 -- \
#       results/handoff/GLSPIKE4/run-all.sh
set -uo pipefail
R=results/handoff/GLSPIKE4/run-arm.sh
G=ACOS_GUEST_GLES=angle-swiftshader
# flag OFF: the default-behaviour gates + the unused baselines
$R off-counter       scenarios/counter.yaml
$R off-a11y-probe    scenarios/a11y-probe.yaml
$R off-a11y-wrap     scenarios/a11y-screenshot-wrap.yaml
# flag ON, GL unused (counter never touches EGL): the "unused-flag cost"
$R on-counter        scenarios/counter.yaml $G
# flag ON, GL used, N=1 (have gles-prod) and N=3 concurrent sessions
for i in 1 2 3; do
  $R gl3-$i scenarios/gles-software.yaml $G &
done
wait
