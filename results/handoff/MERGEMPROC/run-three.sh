#!/bin/bash
set -u
cd "$1" || exit 2
out=$2
mkdir -p "$out"
export ACOS_FONT_DIR=/home/Qui/Projects/acos/.cache/boot/android-34/fonts
export ACOS_VIEW_TEST_DIR=$PWD/.cache/lv
export ACOS_RUNTIME_DIR=$PWD/.cache/rt-mods
export ACOS_SESSION_SOCKET=$PWD/.cache/rt-mods/session.sock
mkdir -p "$ACOS_RUNTIME_DIR"
acos=$PWD/.cache/mergemproc-target/debug/acos
run() {
  name=$1; shift
  dir="$out/$name"; mkdir -p "$dir"
  env -u ACOS_HOME "$acos" verify "$@" --engine art --json > "$dir/verify.json" 2> "$dir/verify.err"
  rc=$?
  status=$(python3 -c "
import json
try:
    d=json.load(open('$dir/verify.json'))
    v=(d.get('result') or {}).get('verdict') or {}
    print('%s verdict=%s steps=%d' % (d.get('status'), v.get('verdict'), len(v.get('steps') or [])))
except Exception as e:
    print('unparseable: %s' % e)
")
  echo "SCENARIO $name rc=$rc $status"
}
run a11y-probe scenarios/a11y-probe.yaml
run location-geocoder results/handoff/MODLOC2/location-geocoder.yaml
run tail-bluetooth results/handoff/MODTAIL/tail-bluetooth.yaml
