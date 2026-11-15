#!/usr/bin/env bash
# FB3 acceptance sweep: 15 gates + 5 Gaten flows + 5 Gaten negatives.
# Every gate is its own run-exclusive acquisition (rule: never hold the lock > ~10 min).
set -uo pipefail
WT=/home/Qui/.t3/worktrees/acos/t3-d41b3b08-5ea0b20149a5a9a9
ACOS="$WT/.cache/fb3/prefix/bin/acos"
RUNEX=/home/Qui/Projects/acos/tools/art-runtime/run-exclusive.sh
GUARD=/home/Qui/Projects/acos/tools/guarded.sh
APK=/home/Qui/Projects/acos/.cache/gaten/gaten-dev-f344272-x86_64-release-unsigned.apk
cd "$WT"
run_one() { # $1 = out dir, $2 = scenario, $3 = optional apk
    local out="$1" s="$2" apk="${3:-}"
    local name; name=$(echo "$s" | tr '/' '_')
    local extra=(); [ -n "$apk" ] && extra=(--apk "$apk")
    "$RUNEX" --task FB3 -- "$GUARD" --mem 10G --time 600 -- \
        "$ACOS" verify "scenarios/$s.yaml" "${extra[@]}" --engine art --artifacts "$out/$name" --json \
        > "$out/$name.json" 2> "$out/$name.err"
    local code=$?
    local verdict
    verdict=$(python3 -I -c "
import json
try: d=json.load(open('$out/$name.json'))
except Exception: print('UNPARSED'); raise SystemExit
print(d.get('status'), (d.get('error') or {}).get('code'))
" 2>/dev/null)
    echo "$s exit=$code status=$verdict"
}
G="$WT/.cache/fb3/gates2"; mkdir -p "$G"
for s in v2-art-counter w22-counter-reset w54-dialog-pixels w42-back-frame w15-dialog-menu \
         form-validation suite/T03-render-good v1-offscreen-pager w7-insets-report \
         se1-expect-change-tap suite/T03-render-wrong w7-insets-carve-negative \
         negative/se1-expect-change-noop negative/offscreen-pager-visible p11-progress-timeout; do
    run_one "$G" "$s"
done
echo "gates sweep done"
E="$WT/.cache/fb3/gaten2"; mkdir -p "$E"
for s in gaten-home gaten-tabs gaten-settings gaten-search gaten-lesson \
         negative/gaten-home-wrong negative/gaten-tabs-wrong negative/gaten-settings-wrong \
         negative/gaten-search-wrong negative/gaten-lesson-wrong; do
    run_one "$E" "$s" "$APK"
done
echo "gaten sweep done"
