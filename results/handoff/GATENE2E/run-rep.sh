#!/usr/bin/env bash
# run-rep.sh <first> <last>: gaten-bubble-overlay repetitions (one lock acquisition)
for i in $(seq "$1" "$2"); do
  results/handoff/GATENE2E/run-arm.sh rep-$i scenarios/gaten-bubble-overlay.yaml
done
