#!/bin/sh
# WVTAIL acceptance runner: N runs of one scenario, one ART acquisition each,
# each run against its **own** broker (endpoint + log under
# .cache/rt-wvtail/<tag>/), so a broker line in the evidence belongs to that run
# and a crash cannot be inherited by the next one.
#
# usage: run-runs.sh <scenario.yaml> <count> <tag>
set -u
cd "$(dirname "$0")/../../.." || exit 1
. .cache/env.sh
scen=${1:?scenario}
n=${2:?count}
tag=${3:?tag}
name=$(basename "$scen" .yaml)
out=results/handoff/WVTAIL/evidence/$tag
mkdir -p "$out"
base=$PWD/.cache/rt-wvtail
i=1
while [ "$i" -le "$n" ]; do
  brok=$base/$tag-$i
  rm -rf "$brok"
  mkdir -p "$brok"
  log=$brok/broker.log
  start=$(date +%s)
  ACOS_WEBVIEW_SOCKET_DIR=$brok/wv \
    tools/art-runtime/run-exclusive.sh --task WVTAIL -- \
    tools/guarded.sh --mem 10G --time 400 -- \
    $ACOS_BIN verify "$scen" --engine art --artifacts "$out/run$i" \
    > "$out/run$i.log" 2>&1
  rc=$?
  end=$(date +%s)
  # Stop the broker this run started (mine, by its own socket path).
  ps -eo pid,args | awk -v s="$brok/broker.sock" 'index($0, s) {print $1}' |
    while read -r p; do kill "$p" 2>/dev/null; done
  status=$(python3 -c "
import json
try:
    print(json.load(open('$out/run$i/verdict.json'))['status'])
except Exception:
    print('NO_VERDICT')")
  ms=$(grep -h "tap -> page click" "$out/run$i/logs/logcat.txt" 2>/dev/null |
    sed 's/.*click //' | head -1)
  slow=$(grep -c "slow op " "$log" 2>/dev/null)
  acks=$(grep -c "NOT acked" "$log" 2>/dev/null)
  restarts=$(grep -c "engine restarted" "$log" 2>/dev/null)
  dies=$(grep -c "engine pid=" "$log" 2>/dev/null)
  cp "$log" "$out/run$i.broker.log" 2>/dev/null
  printf '%-10s run%-3s rc=%s status=%-10s wall=%ss tap=%-8s slowOps=%s noAcks=%s restarts=%s engineDies=%s\n' \
    "$tag" "$i" "$rc" "$status" "$((end-start))" "${ms:-none}" "${slow:-0}" "${acks:-0}" \
    "${restarts:-0}" "${dies:-0}"
  i=$((i + 1))
done
