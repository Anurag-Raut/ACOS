#!/usr/bin/env bash
# GPUSPIKE default-behaviour gates + unused-flag cost (one lock acquisition).
set -uo pipefail
R=results/handoff/GPUSPIKE/run-arm.sh
$R off-counter scenarios/counter.yaml
$R off-a11y-probe scenarios/a11y-probe.yaml
$R off-a11y-wrap scenarios/a11y-screenshot-wrap.yaml
$R on-counter scenarios/counter.yaml ACOS_GUEST_GLES=mesa-host
$R on-counter2 scenarios/counter.yaml ACOS_GUEST_GLES=mesa-host
$R off-counter2 scenarios/counter.yaml
