#!/bin/sh
# WVSCOPE acceptance runner: N runs of one scenario against **one** private
# broker (this branch's build, `ACOS_WEBVIEW_SOCKET_DIR`), which is the point of
# the track: the broker must outlive each session that uses it, so every run
# after the first attaches to the daemon the previous one left behind.
#
# usage: run-runs.sh <scenario.yaml> <count> <tag>
set -u
cd "$(dirname "$0")/../../.." || exit 1
. .cache/env.sh
scen=${1:?scenario}
n=${2:?count}
tag=${3:?tag}
out=results/handoff/WVSCOPE/evidence/$tag
mkdir -p "$out"
log=$ACOS_RUNTIME_DIR/broker.log
i=1
while [ "$i" -le "$n" ]; do
  rm -rf "$out/run$i"
  start=$(wc -l < "$log" 2>/dev/null || echo 0)
  t0=$(date +%s%N)
  tools/art-runtime/run-exclusive.sh --task WVSCOPE -- \
    tools/guarded.sh --mem 10G --time 400 -- \
    $ACOS_BIN verify "$scen" --engine art --artifacts "$out/run$i" \
    > "$out/run$i.log" 2>&1
  rc=$?
  t1=$(date +%s%N)
  status=$(python3 -c "
import json
try:
    print(json.load(open('$out/run$i/verdict.json'))['status'])
except Exception:
    print('NO_VERDICT')")
  tap=$(grep -h "tap -> page click" "$out/run$i/logs/logcat.txt" 2>/dev/null | sed 's/.*click //' | head -1)
  slow=$(grep -h "fair: slow view frame" "$out/run$i/logs/logcat.txt" 2>/dev/null | head -1)
  total=$(wc -l < "$log" 2>/dev/null || echo 0)
  tail -n +$((start + 1)) "$log" > "$out/run$i.broker.log" 2>/dev/null
  printf '%s run%s rc=%s status=%-8s ms=%-6s tap=%-8s %s\n' \
    "$tag" "$i" "$rc" "$status" "$(( (t1 - t0) / 1000000 ))" "${tap:-none}" "${slow:-}"
  i=$((i + 1))
done
# One broker for the batch: stop it at the end (it is this branch's own daemon).
ps -eo pid,args | awk -v s="$ACOS_WEBVIEW_SOCKET_DIR/broker.sock" 'index($0, s) {print $1}' |
  while read -r p; do kill "$p" 2>/dev/null; done
