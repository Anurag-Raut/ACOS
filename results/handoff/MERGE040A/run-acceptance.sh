#!/bin/bash
# MERGE040A acceptance corpus. Call under
#   tools/art-runtime/run-exclusive.sh --task MERGE040A -- tools/guarded.sh -- <this> <wt> <out>
# Every scenario is one `acos verify --engine art --json` run in a private
# runtime dir; the per-scenario verdict, stderr and the whole session tree stay
# in the output dir.
set -u
WT=$1
out=$2
cd "$WT" || exit 2
mkdir -p "$out"
export ACOS_FONT_DIR=/home/Qui/Projects/acos/.cache/boot/android-34/fonts
export ACOS_VIEW_TEST_DIR=$WT/.cache/lv
export ACOS_RUNTIME_DIR=$WT/.cache/rt-merge040a
export ACOS_SESSION_SOCKET=$WT/.cache/rt-merge040a/session.sock
mkdir -p "$ACOS_RUNTIME_DIR" "$ACOS_VIEW_TEST_DIR"
acos=$WT/.cache/merge040a-target/release/acos
fail=0
run() {
  name=$1; shift
  dir="$out/$name"; mkdir -p "$dir"
  start=$(date +%s)
  env -u ACOS_HOME "$acos" verify "$@" --engine art --artifacts "$dir/art" --json \
    > "$dir/verify.json" 2> "$dir/verify.err"
  rc=$?
  secs=$(( $(date +%s) - start ))
  st=$(python3 - "$dir/verify.json" "$name" "$rc" "$secs" <<'PY'
import json,sys
p,name,rc,secs=sys.argv[1:5]
try:
    d=json.load(open(p))
    v=(d.get('result') or {}).get('verdict') or {}
    fs=(d.get('result') or {}).get('failed_step') or {}
    print("SCENARIO %-32s rc=%s %ss status=%s verdict=%s steps=%s failed=%s" % (
        name, rc, secs, d.get('status'), v.get('verdict'), len(v.get('steps') or []), fs.get('kind')))
except Exception as e:
    print("SCENARIO %-32s rc=%s %ss UNPARSEABLE=%s" % (name, rc, secs, e))
PY
)
  echo "$st"
  [ "$rc" -ne 0 ] && fail=$((fail + 1))
}
run a11y-probe                    scenarios/a11y-probe.yaml
run mproc3-two-process            scenarios/mproc3-two-process.yaml
run back-layers                   scenarios/back-layers.yaml
run back-layers-ime               scenarios/back-layers-ime.yaml
run form-validation               scenarios/form-validation.yaml
run looperwake-bg-post            scenarios/looperwake-bg-post.yaml
run rel3h-unhandled-input         scenarios/rel3h-unhandled-input.yaml
run rel3h-expect-change           scenarios/rel3h-unhandled-input-expect-change.yaml
run rel3h2-expect-handled-false   scenarios/rel3h2-expect-handled-false.yaml
run rel3h2-expect-handled-true    scenarios/rel3h2-expect-handled-true.yaml
run touchdisp-dispatch-probe      scenarios/touchdisp-dispatch-probe.yaml
run touchdisp-gesture             scenarios/touchdisp-gesture.yaml
run touchdisp-posted-side-effect  scenarios/touchdisp-posted-side-effect.yaml
run gaten-backstack               scenarios/gaten-backstack.yaml
run gaten-lesson                  scenarios/gaten-lesson.yaml
run gaten-bubble                  scenarios/gaten-bubble.yaml
run gaten-bubble-overlay          scenarios/gaten-bubble-overlay.yaml
run webview-probe                 scenarios/webview-probe.yaml
run webview-fair                  scenarios/webview-fair.yaml
run sysui1-permission-allow       scenarios/sysui1-permission-allow.yaml
run sysui1-permission-deny        scenarios/sysui1-permission-deny.yaml
run sysui1-permission-pregranted  scenarios/sysui1-permission-pregranted.yaml
run sysui1-shade                  scenarios/sysui1-shade.yaml
run notif-granted                 scenarios/notif-perm-granted.yaml
run notif-denied                  scenarios/notif-perm-denied.yaml
run location-attach               results/handoff/MODLOC3/location-live-attach.yaml
run location-geocoder             results/handoff/MODLOC2/location-geocoder.yaml
run location-detach               results/handoff/MODLOC2/location-live-detach.yaml
run sensors-scripted              scenarios/sensors-scripted.yaml
run sensors-off                   scenarios/sensors-off.yaml
run tail-telephony                results/handoff/MODTAIL/tail-telephony.yaml
run tail-biometric                results/handoff/MODTAIL/tail-biometric.yaml
run tail-bluetooth                results/handoff/MODTAIL/tail-bluetooth.yaml
run tail-vibrator                 results/handoff/MODTAIL/tail-vibrator.yaml
run tail-detached                 results/handoff/MODTAIL/tail-detached.yaml
run a11y-screenshot-wrap          scenarios/a11y-screenshot-wrap.yaml
echo "SCENARIO_FAILURES $fail"
exit $fail
