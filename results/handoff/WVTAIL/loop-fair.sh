#!/bin/sh
# WVTAIL: run webview-fair N times against this branch's broker, logging the
# probe's own tap->click number and the broker's frame-gate trace per run.
# One ART acquisition per run (the shared lock is never held for the batch).
set -u
cd "$(dirname "$0")/../../.." || exit 1
. .cache/env.sh
n=${1:-6}
tag=${2:-loop}
out=results/handoff/WVTAIL/evidence/$tag
mkdir -p "$out"
rm -rf "$out"/run*
log=.cache/rt-wvtail/broker.log
i=1
while [ "$i" -le "$n" ]; do
  start=$(wc -l < "$log" 2>/dev/null || echo 0)
  tools/art-runtime/run-exclusive.sh --task WVTAIL -- \
    tools/guarded.sh --mem 10G --time 400 -- \
    $ACOS_BIN verify scenarios/webview-fair.yaml --engine art \
    --artifacts "$out/run$i" > "$out/run$i.log" 2>&1
  rc=$?
  status=$(python3 -c "
import json
try:
    print(json.load(open('$out/run$i/verdict.json'))['status'])
except Exception:
    print('NO_VERDICT')")
  ms=$(grep -h "fair: tap -> page click" "$out/run$i/logs/logcat.txt" 2>/dev/null | sed 's/.*click //' | head -1)
  total=$(wc -l < "$log" 2>/dev/null || echo 0)
  tail -n +$((start + 1)) "$log" > "$out/run$i.gate.log" 2>/dev/null
  printf '%s run%s rc=%s status=%s tap=%s gate_lines=%s\n' "$tag" "$i" "$rc" "$status" "${ms:-none}" "$((total - start))"
  i=$((i + 1))
done
