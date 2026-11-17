#!/bin/bash
# MERGEFIX acceptance corpus (call under tools/art-runtime/run-exclusive.sh).
set -u
cd "$1" || exit 2
out=$2
mkdir -p "$out"
export ACOS_FONT_DIR=/home/Qui/Projects/acos/.cache/boot/android-34/fonts
export ACOS_VIEW_TEST_DIR=$PWD/.cache/lv
export ACOS_RUNTIME_DIR=$PWD/.cache/rt-mergefix
export ACOS_SESSION_SOCKET=$PWD/.cache/rt-mergefix/session.sock
mkdir -p "$ACOS_RUNTIME_DIR"
acos=$PWD/.cache/mergefix-target/release/acos
fail=0
run() {
  name=$1; shift
  dir="$out/$name"; mkdir -p "$dir"
  env -u ACOS_HOME "$acos" verify "$@" --engine art --json > "$dir/verify.json" 2> "$dir/verify.err"
  rc=$?
  st=$(python3 - "$dir/verify.json" "$name" "$rc" <<'PY'
import json,sys
p,name,rc=sys.argv[1],sys.argv[2],sys.argv[3]
try:
    d=json.load(open(p))
    v=(d.get('result') or {}).get('verdict') or {}
    fs=(d.get('result') or {}).get('failed_step') or {}
    print("SCENARIO %s rc=%s status=%s verdict=%s steps=%d failed=%s" % (
        name, rc, d.get('status'), v.get('verdict'), len(v.get('steps') or []), fs.get('kind')))
except Exception as e:
    print("SCENARIO %s rc=%s unparseable=%s" % (name, rc, e))
PY
)
  echo "$st"
  [ "$rc" -ne 0 ] && fail=1
}
run a11y-probe scenarios/a11y-probe.yaml
run mproc3-two-process scenarios/mproc3-two-process.yaml
run looperwake-bg-post scenarios/looperwake-bg-post.yaml
run back-layers scenarios/back-layers.yaml
run form-validation scenarios/form-validation.yaml
run sensors-scripted scenarios/sensors-scripted.yaml
run sensors-off scenarios/sensors-off.yaml
run notif-granted scenarios/notif-perm-granted.yaml
run notif-denied scenarios/notif-perm-denied.yaml
run location-probe results/handoff/MODLOC3/location-live-attach.yaml
run location-geocoder results/handoff/MODLOC2/location-geocoder.yaml
run location-detach results/handoff/MODLOC2/location-live-detach.yaml
run tail-telephony results/handoff/MODTAIL/tail-telephony.yaml
run tail-biometric results/handoff/MODTAIL/tail-biometric.yaml
run tail-bluetooth results/handoff/MODTAIL/tail-bluetooth.yaml
run tail-vibrator results/handoff/MODTAIL/tail-vibrator.yaml
run tail-detached results/handoff/MODTAIL/tail-detached.yaml
run webview-probe scenarios/webview-probe.yaml
run gaten-backstack scenarios/gaten-backstack.yaml
echo "SCENARIO_FAILURES $fail"
exit $fail
