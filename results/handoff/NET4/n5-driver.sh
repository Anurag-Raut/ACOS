#!/usr/bin/env bash
# NET3: N concurrent net-check verifies on real ART, relay arm, with the stage
# table of every run recorded. Prints one line per rep and one summary.
#
#   ACOS_NET_PASTA=… results/handoff/NET4/n5-driver.sh OUTDIR [reps] [n]
#
# Must run inside `tools/art-runtime/run-exclusive.sh -- tools/guarded.sh -- …`.
set -u
HERE=$(cd "$(dirname "$0")" && pwd)
WT=$(cd "$HERE/../../.." && pwd)
OUT=${1:?outdir}
REPS=${2:-1}
N=${3:-5}
BIN=${ACOS:-$WT/.cache/net3-target/release/acos}
export ACOS_FONT_DIR=${ACOS_FONT_DIR:-/home/Qui/Projects/acos/.cache/boot/android-34/fonts}
export ACOS_SPIKE_NET=1 ACOS_NET_ARM=relay ACOS_ART_STAGE_TIMING=1
export ACOS_NETD_BIN=${ACOS_NETD_BIN:-$WT/.cache/net3-target/release/acos-netd}
mkdir -p "$OUT"

for rep in $(seq 1 "$REPS"); do
  rdir=$OUT/rep$rep
  rm -rf "$rdir"; mkdir -p "$rdir"
  for i in $(seq 1 "$N"); do
    ( cd "$WT" && env -u ACOS_HOME "$BIN" verify --engine art \
        --apk fixtures/build/apps/net-check.apk --scenario scenarios/net-check.yaml \
        --artifacts "$rdir/$i" >"$rdir/$i.out" 2>"$rdir/$i.err"; \
      echo "exit=$?" >>"$rdir/$i.out" ) &
  done
  wait
  summary=$rdir/summary.txt
  : >"$summary"
  for i in $(seq 1 "$N"); do
    ok=$(grep -c 'verify: pass' "$rdir/$i.out" 2>/dev/null || true)
    steps=$(grep -o 'steps: [0-9]*' "$rdir/$i.out" 2>/dev/null | tail -1 | grep -o '[0-9]*')
    why=$(grep -o 'verify: error[^;]*' "$rdir/$i.out" 2>/dev/null | head -1)
    why=$(grep -o 'error: [A-Z_]*: [^[]*' "$rdir/$i.out" 2>/dev/null | head -1)
    echo "s$i pass=$ok steps=${steps:-0} ${why}" >>"$summary"
  done
  pass=$(grep -c 'pass=1' "$summary" || true)
  full=$(grep -c 'steps=10' "$summary" || true)
  echo "rep$rep: pass=$pass/$N steps10=$full/$N" | tee -a "$OUT/n5.log"
  # The stage tables, one per run (stderr markers, gap in ms).
  for i in $(seq 1 "$N"); do
    awk '/acos-art: stage /{for(j=1;j<=NF;j++){if($j ~ /^exec=[0-9]+ms$/){gsub(/[^0-9]/,"",$j); t=$j+0}}; sub(/.*stage /,""); sub(/ exec=.*/,""); if (have) printf "%s +%dms\n", $0, t-prev; else printf "%s %dms\n", $0, t; prev=t; have=1}' \
      "$rdir/$i.err" >"$rdir/$i.stages" 2>/dev/null
  done
done
cat "$summary"
