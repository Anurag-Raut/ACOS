#!/usr/bin/env bash
# P11 settle-lab corpus under the P12 shipped default (event strict, pixel
# guard OFF, animations on). A pass when the scenario expects non_pass/fail is
# a false pass and fails this script.
set -u
ACOS="$1"; ROOT="$2"; ART="$3"
export ACOS_ART_BIN="$ART"
mkdir -p "$ROOT"
false_passes=0
failures=0
for s in scenarios/p11-*.yaml; do
  name=$(basename "$s" .yaml)
  d="$ROOT/$name"; mkdir -p "$d"
  apk=$(grep -m1 '^app:' "$s" | sed 's/^app:[[:space:]]*//')
  expect=$(grep -m1 '^expectation:' "$s" | awk '{print $2}')
  expect=${expect:-pass}
  "$ACOS" verify --scenario "$s" --apk "$apk" --engine art --animations on \
      --artifacts "$d" --json >"$d/out.json" 2>"$d/err.log"
  verdict=$(python3 -c "import json;d=json.load(open('$d/out.json'));v=(d.get('result') or {}).get('verdict') or {};print(v.get('verdict','error'))" 2>/dev/null || echo error)
  if [ "$expect" = pass ]; then
    if [ "$verdict" = pass ]; then echo "PASS $name (expected pass)"; else echo "FAIL $name (expected pass, got $verdict)"; failures=$((failures+1)); fi
  else
    if [ "$verdict" = pass ]; then echo "FALSE-PASS $name (expected $expect)"; false_passes=$((false_passes+1)); else echo "PASS $name (expected non-pass, got $verdict)"; fi
  fi
done
echo "false_passes=$false_passes unexpected_failures=$failures"
[ "$false_passes" = 0 ]
