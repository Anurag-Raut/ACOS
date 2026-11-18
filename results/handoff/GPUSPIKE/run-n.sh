#!/usr/bin/env bash
# GPUSPIKE: N=6 concurrent GL-used sessions on the one host GPU (egl-probe incl. bench).
set -uo pipefail
R=results/handoff/GPUSPIKE/run-arm.sh
for i in 1 2 3 4 5 6; do
  $R n6-$i scenarios/egl-probe.yaml ACOS_GUEST_GLES=mesa-host &
done
wait
