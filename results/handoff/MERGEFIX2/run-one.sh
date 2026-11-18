#!/bin/bash
# MERGEFIX2: run ONE scenario against the tree's release binaries in a private
# runtime dir. Call it under
#   tools/art-runtime/run-exclusive.sh --task MERGEFIX2 -- tools/guarded.sh -- <this> <scenario> <outdir> [extra env...]
# Usage: run-one.sh scenarios/gaten-backstack.yaml .cache/runs/<tag>/<name>
set -u
WT=$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)
scenario=$1
out=$2
tag=${MERGEFIX2_TAG:-mf2}
cd "$WT" || exit 2
name=$(basename "$scenario" .yaml)
mkdir -p "$out/$name" "$WT/.cache/rt-$tag" "$WT/.cache/lv"
export ACOS_FONT_DIR=/home/Qui/Projects/acos/.cache/boot/android-34/fonts
export ACOS_VIEW_TEST_DIR=$WT/.cache/lv
export ACOS_RUNTIME_DIR=$WT/.cache/rt-$tag
export ACOS_SESSION_SOCKET=$WT/.cache/rt-$tag/session.sock
acos=${ACOS_BIN:-$WT/.cache/$tag-target/release/acos}
export ACOS_ART_BIN=${ACOS_ART_BIN:-$WT/.cache/$tag-target/release/acos-art}
[ -x "$acos" ] || { echo "no acos binary at $acos" >&2; exit 2; }
start=$(date +%s)
env -u ACOS_HOME "$acos" verify "$scenario" --engine art --artifacts "$out/$name/art" \
    --json ${EXTRA_VERIFY_ARGS:-} > "$out/$name/verify.json" 2> "$out/$name/verify.err"
rc=$?
secs=$(( $(date +%s) - start ))
python3 - "$out/$name/verify.json" "$name" "$rc" "$secs" <<'PY'
import json,sys
p,name,rc,secs=sys.argv[1:5]
try:
    d=json.load(open(p))
    v=(d.get('result') or {}).get('verdict') or {}
    fs=(d.get('result') or {}).get('failed_step') or {}
    print("SCENARIO %-28s rc=%s %ss status=%s verdict=%s steps=%s failed=%s" % (
        name, rc, secs, d.get('status'), v.get('verdict'), len(v.get('steps') or []), fs.get('kind')))
    err=(d.get('result') or {}).get('failure') or d.get('error') or {}
    det=((err or {}).get('error') or (err or {})).get('details') or {}
    if det:
        print("  reason=%s settle=%s" % (det.get('reason'), json.dumps((det.get('settle') or {}).get('reason'))))
except Exception as e:
    print("SCENARIO %-28s rc=%s %ss UNPARSEABLE=%s" % (name, rc, secs, e))
PY
exit $rc
