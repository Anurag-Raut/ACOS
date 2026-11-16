#!/usr/bin/env bash
# The control arm of the N=5 flake: the same five net-check verifies with the
# network module OFF (`ACOS_SPIKE_NET` unset). The scenario expects a pass, so
# every run fails its fetch; what this measures is the *tree binding* stage that
# failed in NET2 (GUEST_EXITED) and after the proxy fix 
# (strict-settle timeout): does it flake without the relay at all?
set -u
HERE=$(cd "$(dirname "$0")" && pwd)
WT=$(cd "$HERE/../../.." && pwd)
OUT=${1:?outdir}; REPS=${2:-1}; N=${3:-5}
BIN=${ACOS:-$WT/.cache/net3-target/release/acos}
export ACOS_FONT_DIR=${ACOS_FONT_DIR:-/home/Qui/Projects/acos/.cache/boot/android-34/fonts}
export ACOS_ART_STAGE_TIMING=1
unset ACOS_SPIKE_NET ACOS_NET_ARM
mkdir -p "$OUT"
for rep in $(seq 1 "$REPS"); do
  rdir=$OUT/rep$rep; rm -rf "$rdir"; mkdir -p "$rdir"
  for i in $(seq 1 "$N"); do
    ( cd "$WT" && env -u ACOS_HOME "$BIN" verify --engine art \
        --apk fixtures/build/apps/net-check.apk --scenario scenarios/net-check.yaml \
        --artifacts "$rdir/$i" >"$rdir/$i.out" 2>"$rdir/$i.err"; echo "exit=$?" >>"$rdir/$i.out" ) &
  done
  wait
  : >"$rdir/summary.txt"
  for i in $(seq 1 "$N"); do
    steps=$(grep -o 'steps: [0-9]*' "$rdir/$i.out" 2>/dev/null | tail -1 | grep -o '[0-9]*')
    why=$(grep -o 'error: [A-Z_]*: [^[]*' "$rdir/$i.out" 2>/dev/null | head -1)
    echo "s$i steps=${steps:-0} ${why}" >>"$rdir/summary.txt"
  done
  echo "rep$rep net-off: steps10=$(grep -c 'steps=10' "$rdir/summary.txt")/$N" | tee -a "$OUT/n5-netoff.log"
  cat "$rdir/summary.txt" | tee -a "$OUT/n5-netoff.log"
done
