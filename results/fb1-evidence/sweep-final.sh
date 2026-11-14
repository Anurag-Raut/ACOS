#!/usr/bin/env bash
# FB1 final acceptance (on commit 1f53bde's installed prefix): the 15 gates and the
# 5 owner Gaten flows + 5 negatives. One process, one run-exclusive acquisition.
set -uo pipefail
WT=/home/Qui/.paseo/worktrees/3oxa6xao/s3ab
ACOS=$WT/.cache/fb1/prefix/bin/acos
OUT=$WT/.cache/fb1/final-sweep
mkdir -p "$OUT"
export ACOS_ARTIFACTS_DIR=$OUT/artifacts
GATEN_APK=/home/Qui/Projects/acos/.cache/gaten/gaten-dev-f344272-x86_64-release-unsigned.apk

run() {
  local expected=$1 name=$2
  local dir="$OUT/artifacts/$name"
  mkdir -p "$dir" "$(dirname "$OUT/$name.json")"
  local t0=$SECONDS
  "$ACOS" verify "$WT/scenarios/$name.yaml" --engine art --artifacts "$dir" --json \
      > "$OUT/$name.json" 2>"$OUT/$name.err"
  local rc=$?
  local status code
  status=$(python3 -c "import json;d=json.load(open('$dir/verdict.json'));print(d.get('verdict') or d.get('status'))" 2>/dev/null)
  code=$(python3 -c "import json;d=json.load(open('$dir/verdict.json'));print((d.get('failure') or {}).get('code') or '')" 2>/dev/null)
  local ok=MISMATCH
  if [ "$status" = "$expected" ] || { [ "$expected" = pass ] && [ "$status" = pass ]; }; then ok=ok; fi
  printf '%-42s %-10s %-18s expected=%-18s rc=%s %ss %s\n' "$name" "$status" "$code" "$expected" "$rc" "$((SECONDS-t0))" "$ok" \
      | tee -a "$OUT/summary.txt"
}

# Usage: sweep-final.sh <chunk>   (chunk: A|B|C|D; each chunk is one lock acquisition < 10 min)
case "${1:-}" in
  A) run pass v2-art-counter; run pass w22-counter-reset; run pass w54-dialog-pixels;
     run pass w42-back-frame; run pass w15-dialog-menu ;;
  B) run pass form-validation; run pass suite/T03-render-good; run pass v1-offscreen-pager;
     run pass w7-insets-report; run pass se1-expect-change-tap ;;
  C) run ASSERTION_FAILED suite/T03-render-wrong; run ASSERTION_FAILED w7-insets-carve-negative;
     run ASSERTION_FAILED negative/se1-expect-change-noop; run ELEMENT_OFFSCREEN negative/offscreen-pager-visible;
     run TIMEOUT p11-progress-timeout ;;
  D) for s in gaten-home gaten-tabs gaten-settings gaten-search gaten-lesson; do run pass "$s"; done
     for s in gaten-home-wrong gaten-tabs-wrong gaten-settings-wrong gaten-search-wrong gaten-lesson-wrong; do
       run ASSERTION_FAILED "negative/$s"; done ;;
  *) echo "usage: $0 A|B|C|D"; exit 2 ;;
esac
echo "CHUNK ${1} DONE"
