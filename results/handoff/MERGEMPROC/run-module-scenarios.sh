#!/bin/bash
# MERGEMPROC: the module scenario corpus on the merged tree (one ART acquisition).
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
fail=0
run() {
  name=$1; shift
  dir="$out/$name"; mkdir -p "$dir"
  env -u ACOS_HOME "$acos" verify "$@" --engine art --json > "$dir/verify.json" 2> "$dir/verify.err"
  rc=$?
  status=$(python3 -c "
import json,sys
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
run net-check scenarios/net-check.yaml
run net-check-offline scenarios/net-check-offline.yaml
run net-network-real scenarios/net-network-real.yaml
run net-network-off scenarios/net-network-offline.yaml
echo "SCENARIO_FAILURES $fail"
exit $fail
