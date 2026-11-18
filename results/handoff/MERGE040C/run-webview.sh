#!/bin/sh
# MERGE040C: run one webview scenario N times, one ART acquisition per run, each
# against its **own** broker (endpoint + log under .cache/wv-mc/<tag>-<i>/), so a
# broker line belongs to that run and a restart cannot be inherited.
#
# usage: run-webview.sh <scenario.yaml> <count> <tag>
set -u
cd "$(dirname "$0")/../../.." || exit 1
. .cache/env.sh
scen=${1:?scenario}
n=${2:?count}
tag=${3:?tag}
name=$(basename "$scen" .yaml)
out=results/handoff/MERGE040C/art/$name-$tag
mkdir -p "$out"
base=$PWD/.cache/wv-mc
i=1
while [ "$i" -le "$n" ]; do
  brok=$base/$tag-$i
  rm -rf "$brok" "$out/run$i"
  mkdir -p "$brok"
  log=$brok/broker.log
  start=$(date +%s)
  ACOS_WEBVIEW_SOCKET_DIR=$brok/wv \
    tools/art-runtime/run-exclusive.sh --task MERGE040C -- \
    tools/guarded.sh --mem 10G --time 400 -- \
    "$ACOS_BIN" verify "$scen" --engine art --artifacts "$out/run$i" \
    > "$out/run$i.log" 2>&1
  rc=$?
  end=$(date +%s)
  ps -eo pid,args | awk -v s="$brok/broker.sock" 'index($0, s) {print $1}' |
    while read -r p; do kill "$p" 2>/dev/null; done
  status=$(python3 - "$out/run$i" <<'PY'
import json, sys
try:
    v = json.load(open(sys.argv[1] + "/verdict.json"))
    print(v.get("status"))
except Exception:
    print("NO_VERDICT")
PY
)
  ms=$(grep -h "tap -> page click" "$out/run$i/logs/logcat.txt" 2>/dev/null |
    sed 's/.*click //' | head -1)
  acks=$(grep -c "NOT acked" "$log" 2>/dev/null)
  restarts=$(grep -c "engine restarted" "$log" 2>/dev/null)
  cp "$log" "$out/run$i.broker.log" 2>/dev/null
  printf '%-16s run%-2s rc=%s status=%-10s wall=%ss tap=%-8s noAcks=%s restarts=%s\n' \
    "$tag" "$i" "$rc" "$status" "$((end - start))" "${ms:-none}" "${acks:-0}" \
    "${restarts:-0}"
  i=$((i + 1))
done
