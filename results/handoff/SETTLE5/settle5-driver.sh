#!/usr/bin/env bash
# SETTLE5: N concurrent `acos verify` sessions of the same idle-screen scenario,
# each with its own host/guest trace, so a strict-settle `probe-floor` timeout
# can be attributed to a guest sample, a missing wake or a host wait.
#
#   results/handoff/SETTLE5/settle5-driver.sh OUTDIR [reps] [n]
#
# Must run inside `tools/art-runtime/run-exclusive.sh -- tools/guarded.sh -- …`.
set -u
HERE=$(cd "$(dirname "$0")" && pwd)
WT=$(cd "$HERE/../../.." && pwd)
OUT=${1:?outdir}
REPS=${2:-3}
N=${3:-5}
BIN=${ACOS:-$WT/.cache/settle5-target/release/acos}
APK=${APK:-fixtures/build/apps/counter.apk}
SCENARIO=${SCENARIO:-$HERE/settle5-probe.yaml}
export ACOS_FONT_DIR=${ACOS_FONT_DIR:-/home/Qui/Projects/acos/.cache/boot/android-34/fonts}
export ACOS_ART_STAGE_TIMING=1
export ACOS_RUNTIME_DIR=${ACOS_RUNTIME_DIR:-$OUT/rt}
export ACOS_SESSION_SOCKET=${ACOS_SESSION_SOCKET:-$ACOS_RUNTIME_DIR/session.sock}
export ACOS_VIEW_TEST_DIR=${ACOS_VIEW_TEST_DIR:-$WT/.cache/lv}
mkdir -p "$OUT" "$ACOS_RUNTIME_DIR"

for rep in $(seq 1 "$REPS"); do
  rdir=$OUT/rep$rep
  rm -rf "$rdir"; mkdir -p "$rdir"
  for i in $(seq 1 "$N"); do
    mkdir -p "$rdir/$i"
    (
      cd "$WT" &&
      env -u ACOS_HOME \
        ACOS_ART_INPUT_TRACE="$rdir/$i/trace.log" \
        ACOSLAUNCH_GUEST_STDOUT_FILE="$rdir/$i/guest-stdout.log" \
        ACOSLAUNCH_GUEST_STDERR_FILE="$rdir/$i/guest-stderr.log" \
        "$BIN" verify --engine art --apk "$APK" --scenario "$SCENARIO" \
          --artifacts "$rdir/$i" >"$rdir/$i.out" 2>"$rdir/$i.err"
      echo "exit=$?" >>"$rdir/$i.out"
    ) &
  done
  wait
  summary=$rdir/summary.txt
  : >"$summary"
  for i in $(seq 1 "$N"); do
    ok=$(grep -c 'verify: pass' "$rdir/$i.out" 2>/dev/null || true)
    steps=$(grep -o 'steps: [0-9]*' "$rdir/$i.out" 2>/dev/null | tail -1 | grep -o '[0-9]*')
    why=$(grep -o 'error: [A-Z_]*: [^[]*' "$rdir/$i.out" 2>/dev/null | head -1)
    echo "s$i pass=$ok steps=${steps:-0} ${why}" >>"$summary"
  done
  pass=$(grep -c 'pass=1' "$summary" || true)
  echo "rep$rep: pass=$pass/$N" | tee -a "$OUT/n5.log"
  for i in $(seq 1 "$N"); do
    awk '/acos-art: stage /{for(j=1;j<=NF;j++){if($j ~ /^exec=[0-9]+ms$/){gsub(/[^0-9]/,"",$j); t=$j+0}}; sub(/.*stage /,""); sub(/ exec=.*/,""); if (have) printf "%s +%dms\n", $0, t-prev; else printf "%s %dms\n", $0, t; prev=t; have=1}' \
      "$rdir/$i.err" >"$rdir/$i.stages" 2>/dev/null
  done
done
cat "$OUT"/rep*/summary.txt
