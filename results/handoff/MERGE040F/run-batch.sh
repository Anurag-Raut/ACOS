#!/usr/bin/env bash
# MERGE040F: run proof scenarios on ART, one artifacts dir + JSON per scenario.
set -uo pipefail
WT=$(pwd)
BIN=$WT/.cache/m4f-rel/release/acos
OUT=$WT/results/handoff/MERGE040F/evidence
mkdir -p "$OUT" "$WT/.cache/rt"
cd "$WT" || exit 2
rc_all=0
for s in "$@"; do
  name=$(basename "$s" .yaml)
  dir="$OUT/$name"
  rm -rf "$dir"
  if [ -f "$s" ]; then scenario=$s; else scenario="scenarios/$s.yaml"; fi
  echo "=== $name ($scenario) $(date -u +%FT%TZ)"
  env -u ACOS_HOME \
      ACOS_ART_DATA="$WT/.cache/m4f-data" \
      ACOS_FONT_DIR=/home/Qui/Projects/acos/.cache/boot/android-34/fonts \
      ACOS_RUNTIME_DIR="$WT/.cache/rt" \
      ACOS_SESSION_SOCKET="$WT/.cache/rt/session.sock" \
      ACOS_VIEW_TEST_DIR="$WT/.cache/lv" \
      ACOS_ART_BIN="$WT/.cache/m4f-rel/release/acos-art" \
      ACOS_WEBVIEW_CACHE=/home/Qui/Projects/acos/.cache/webview \
      PATH="$WT/.cache/m4f-rel/release:$PATH" \
      "$BIN" verify "$scenario" --engine art --artifacts "$dir" --json \
      > "$OUT/$name.json" 2> "$OUT/$name.err"
  rc=$?
  status=$(python3 -c "import json,sys;d=json.load(open('$OUT/$name.json'));print(d.get('status'), d.get('summary','')[:80])" 2>/dev/null || echo "unparsable")
  echo "--- $name rc=$rc status=$status"
  [ $rc -eq 0 ] || rc_all=1
done
exit $rc_all
