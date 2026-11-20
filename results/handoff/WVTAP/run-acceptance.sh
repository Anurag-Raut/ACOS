#!/bin/sh
# WVTAP acceptance batch: the two WebView scenarios 10x each, then the
# regression set that the input-routing change must not break.
# One ART acquisition per run (the shared lock is never held for the batch).
set -u
cd /home/Qui/Projects/acos/.cache/wt-wvtap || exit 1
. .cache/env.sh
BIN=./.cache/wvtap-target/release/acos
run_one() {
  scen="$1"; out="$2"
  mkdir -p "$(dirname "$out")"
  rm -rf "$out"
  start=$(date +%s)
  tools/art-runtime/run-exclusive.sh --task WVTAP -- \
    tools/guarded.sh --mem 10G --time 400 -- \
    $BIN verify "$scen" --engine art --artifacts "$out" > "$out.log" 2>&1
  rc=$?
  end=$(date +%s)
  status=$(python3 -c "
import json
try:
    v=json.load(open('$out/verdict.json'));print(v['status'])
except Exception:
    print('NO_VERDICT')
")
  printf '%-28s rc=%s status=%-10s wall=%ss\n' "$(basename "$scen" .yaml)" "$rc" "$status" "$((end-start))"
}

echo '=== webview-fair x10'
i=1
while [ $i -le 10 ]; do
  run_one scenarios/webview-fair.yaml "results/handoff/WVTAP/evidence/fair10b/run$i"
  i=$((i+1))
done
echo '=== webview-probe x10'
i=1
while [ $i -le 10 ]; do
  run_one scenarios/webview-probe.yaml "results/handoff/WVTAP/evidence/probe10c/run$i"
  i=$((i+1))
done
echo '=== regressions'
for s in a11y-probe gaten-bubble touchdisp-gesture touchdisp-dispatch-probe \
         touchdisp-posted-side-effect rel3s-gaten-scroll; do
  run_one "scenarios/$s.yaml" "results/handoff/WVTAP/evidence/regress/$s"
done
echo '=== done'
