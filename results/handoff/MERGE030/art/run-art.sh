#!/bin/bash
# MERGE030: the accepted ART corpus on the merged tree (call under run-exclusive).
set -u
cd "$1" || exit 2
out=$2
mkdir -p "$out"
export ACOS_FONT_DIR=/home/Qui/Projects/acos/.cache/boot/android-34/fonts
export ACOS_VIEW_TEST_DIR=$PWD/.cache/lv
export ACOS_RUNTIME_DIR=$PWD/.cache/rt-merge030
export ACOS_SESSION_SOCKET=$PWD/.cache/rt-merge030/session.sock
mkdir -p "$ACOS_RUNTIME_DIR"
acos=$PWD/.cache/merge030-target/release/acos
fail=0
run() {
  name=$1; shift
  dir="$out/$name"; mkdir -p "$dir"
  env -u ACOS_HOME "$acos" verify "$@" --engine art --json > "$dir/verify.json" 2> "$dir/verify.err"
  rc=$?
  status=$(python3 -c "
import json
try:
    d=json.load(open('$dir/verify.json'))
    v=(d.get('result') or {}).get('verdict') or d.get('verdict') or {}
    print('%s verdict=%s steps=%d unsupported=%s' % (d.get('status'), v.get('verdict'), len(v.get('steps') or []), v.get('unsupported_steps')))
except Exception as e:
    print('unparseable: %s' % e)
")
  echo "SCENARIO $name rc=$rc $status"
  if [ $rc -ne 0 ]; then fail=1; fi
}
run back-layers scenarios/back-layers.yaml
run back-layers-ime scenarios/back-layers-ime.yaml
run rel3h-unhandled scenarios/rel3h-unhandled-input.yaml
run rel3h-unhandled-expect scenarios/rel3h-unhandled-input-expect-change.yaml
run rel3h2-expect-true scenarios/rel3h2-expect-handled-true.yaml
run rel3h2-expect-false scenarios/rel3h2-expect-handled-false.yaml
run touchdisp-dispatch scenarios/touchdisp-dispatch-probe.yaml
run touchdisp-gesture scenarios/touchdisp-gesture.yaml
run touchdisp-side-effect scenarios/touchdisp-posted-side-effect.yaml
run form-validation scenarios/form-validation.yaml
run gaten-backstack scenarios/gaten-backstack.yaml
run gaten-lesson scenarios/gaten-lesson.yaml
run rel3a-content-settle scenarios/rel3a-content-settle.yaml
run rel3a-content-pixel scenarios/rel3a-content-pixel.yaml
run rel3a-content-sticky scenarios/rel3a-content-sticky.yaml
run looperwake-bg-post scenarios/looperwake-bg-post.yaml
run sensors-scripted scenarios/sensors-scripted.yaml
run sensors-off scenarios/sensors-off.yaml
run a11y-probe scenarios/a11y-probe.yaml
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
run mproc3-two-process scenarios/mproc3-two-process.yaml
run net-network-off scenarios/net-network-offline.yaml
run net-check scenarios/net-check.yaml
run net-check-offline scenarios/net-check-offline.yaml
echo "SCENARIO_FAILURES $fail"
exit $fail
