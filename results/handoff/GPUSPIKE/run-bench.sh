#!/usr/bin/env bash
# GPUSPIKE per-frame / init cost: egl-probe's bench on SwiftShader vs the host GPU,
# interleaved (A B A B A B), shared slots (NOT exclusive: indicative only).
set -uo pipefail
R=results/handoff/GPUSPIKE/run-arm.sh
for i in 1 2 3; do
  $R bench-mesa-$i scenarios/egl-probe.yaml ACOS_GUEST_GLES=mesa-host
  $R bench-swiftshader-$i scenarios/egl-probe.yaml ACOS_GUEST_GLES=angle-swiftshader
done
