#!/usr/bin/env bash
# MERGE040H: run scenarios on ART, one artifacts dir + JSON each (per-scenario exclusive lock).
set -uo pipefail
WT=$(cd "$(dirname "$0")/../../.." && pwd)
T=${M40H_BIN:-$WT/.cache/m40h-target/release}
OUT=$WT/results/handoff/MERGE040H/evidence
mkdir -p "$OUT" "$WT/.cache/rt"; cd "$WT" || exit 2
rc_all=0
for s in "$@"; do
  name=$(basename "$s" .yaml); dir="$OUT/$name"; rm -rf "$dir"
  if [ -f "$s" ]; then scenario=$s; else scenario="scenarios/$s.yaml"; fi
  echo "=== $name $(date -u +%FT%TZ)"
  tools/art-runtime/run-exclusive.sh --task MERGE040H -- env -u ACOS_HOME \
    ACOS_FONT_DIR=/home/Qui/Projects/acos/.cache/boot/android-34/fonts ACOS_WEBVIEW_CACHE=/home/Qui/Projects/acos/.cache/webview \
    ACOS_RUNTIME_DIR="$WT/.cache/rt" ACOS_SESSION_SOCKET="$WT/.cache/rt/session.sock" ACOS_VIEW_TEST_DIR="$WT/.cache/lv" \
    ACOS_NET_PASTA=/home/Qui/Projects/acos/.cache/pasta-pinned/pasta ACOS_ART_SYSROOT=/home/Qui/Projects/acos/.cache/art/sysroot ACOS_ART_BIN="$T/acos-art" PATH="$T:$PATH" \
    tools/guarded.sh --mem 10G --time 600 -- "$T/acos" verify "$scenario" --engine art --artifacts "$dir" --json \
    > "$OUT/$name.json" 2> "$OUT/$name.err"
  rc=$?
  status=$(python3 -c "import json;d=json.load(open('$OUT/$name.json'));print(d.get('status'), str(d.get('summary',''))[:100])" 2>/dev/null || echo unparsable)
  echo "--- $name rc=$rc status=$status"; [ $rc -eq 0 ] || rc_all=1
done
exit $rc_all
