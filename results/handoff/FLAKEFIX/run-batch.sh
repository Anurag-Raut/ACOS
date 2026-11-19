#!/usr/bin/env bash
# FLAKEFIX: run scenarios on ART, one artifacts dir + JSON each, N repetitions.
#   run-batch.sh [-n REPS] [-o OUTDIR] scenario...   (no ACOS_ART_SYSROOT override on purpose)
set -uo pipefail
WT=$(cd "$(dirname "$0")/../../.." && pwd)
T=${FF_BIN:-${FF2_BIN:-$WT/.cache/ff3-target/release}}
REPS=1; OUT=$WT/.cache/ff3-evidence
while getopts "n:o:" o; do case $o in n) REPS=$OPTARG;; o) OUT=$OPTARG;; esac; done; shift $((OPTIND-1))
STREAM=${FF_STREAM:-${FF2_STREAM:-0}}
RT=$WT/.cache/rt-ff3-$STREAM
TASK=${FF_TASK:-FLAKEFIX3}
mkdir -p "$OUT" "$RT"; cd "$WT" || exit 2
rc_all=0
for s in "$@"; do
  name=$(basename "$s" .yaml)
  if [ -f "$s" ]; then scenario=$s; else scenario="scenarios/$s.yaml"; fi
  for i in $(seq 1 "$REPS"); do
    dir="$OUT/$name/run$i"; rm -rf "$dir"; mkdir -p "$OUT/$name"
    tools/art-runtime/run-exclusive.sh --task "$TASK" -- env -u ACOS_HOME -u ACOS_ART_SYSROOT \
      ACOS_FONT_DIR=/home/Qui/Projects/acos/.cache/boot/android-34/fonts \
      ACOS_WEBVIEW_CACHE=/home/Qui/Projects/acos/.cache/webview \
      ACOS_RUNTIME_DIR="$RT" ACOS_SESSION_SOCKET="$RT/session.sock" ACOS_VIEW_TEST_DIR="$WT/.cache/lv" \
      ACOS_NET_PASTA=/home/Qui/Projects/acos/.cache/pasta-pinned/pasta ACOS_ART_BIN="$T/acos-art" PATH="$T:$PATH" \
      tools/guarded.sh --mem 8G --time 600 -- "$T/acos" verify "$scenario" --engine art --artifacts "$dir" --json \
      > "$OUT/$name/run$i.json" 2> "$OUT/$name/run$i.err"
    rc=$?
    status=$(python3 -c "import json;d=json.load(open('$OUT/$name/run$i.json'));print(d.get('status'), str((d.get('metrics') or {}).get('wall_ms','?'))+'ms', str(d.get('error',{}).get('code',''))[:40])" 2>/dev/null || echo unparsable)
    echo "--- $name run$i rc=$rc status=$status"; [ $rc -eq 0 ] || rc_all=1
  done
done
exit $rc_all
