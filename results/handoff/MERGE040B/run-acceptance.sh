#!/bin/bash
# MERGE040B acceptance corpus. Call under
#   tools/art-runtime/run-exclusive.sh --task MERGE040B -- tools/guarded.sh -- <this> <wt> <out> <group>
# Every scenario is one `acos verify --engine art --json` run in a private
# runtime dir; the verdict, stderr and the whole session tree stay in <out>.
#
# Groups (so one lock acquisition stays well under ~10 min):
#   core      counter, form-validation, back-layers(+ime), gaten-*, rel3s, a11y,
#             touchdisp-*, looperwake
#   proc      mproc3, mproc8 bind-only, binderpid, launch-extras(+parcelable)
#   web       webview-probe x3, webview-fair x3
#   sysui     sysui1 permission allow/deny/pregranted/shade, notif x2
#   devices   location x3, sensors x2
set -u
WT=$1
out=$2
group=${3:-core}
cd "$WT" || exit 2
mkdir -p "$out"
export ACOS_FONT_DIR=/home/Qui/Projects/acos/.cache/boot/android-34/fonts
export ACOS_VIEW_TEST_DIR=$WT/.cache/lv
export ACOS_RUNTIME_DIR=$WT/.cache/rt-merge040b
export ACOS_SESSION_SOCKET=$WT/.cache/rt-merge040b/session.sock
mkdir -p "$ACOS_RUNTIME_DIR" "$ACOS_VIEW_TEST_DIR"
acos=$WT/.cache/merge040b-target/release/acos
fail=0
run() {
  name=$1; shift
  dir="$out/$name"
  # A rep re-uses its directory name on purpose: clear the previous run's
  # artifacts so the run owns a fresh one (never --overwrite silently in place).
  rm -rf "$dir"; mkdir -p "$dir"
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
    err=(d.get('error') or {})
    cls=(err.get('code') or {}).get('class') if isinstance(err.get('code'), dict) else err.get('class')
    print("SCENARIO %-34s rc=%s %ss status=%s verdict=%s steps=%s failed=%s class=%s" % (
        name, rc, secs, d.get('status'), v.get('verdict'), len(v.get('steps') or []), fs.get('kind'), cls))
except Exception as e:
    print("SCENARIO %-34s rc=%s %ss UNPARSEABLE=%s" % (name, rc, secs, e))
PY
)
  echo "$st"
  [ "$rc" -ne 0 ] && fail=$((fail + 1))
}

case "$group" in
core)
  run counter                       scenarios/counter.yaml
  run form-validation               scenarios/form-validation.yaml
  run back-layers                   scenarios/back-layers.yaml
  run back-layers-ime               scenarios/back-layers-ime.yaml
  run gaten-backstack               scenarios/gaten-backstack.yaml
  run gaten-lesson                  scenarios/gaten-lesson.yaml
  run gaten-bubble                  scenarios/gaten-bubble.yaml
  run rel3s-gaten-scroll            scenarios/rel3s-gaten-scroll.yaml
  run a11y-probe                    scenarios/a11y-probe.yaml
  run looperwake-bg-post            scenarios/looperwake-bg-post.yaml
  run touchdisp-dispatch-probe      scenarios/touchdisp-dispatch-probe.yaml
  run touchdisp-gesture             scenarios/touchdisp-gesture.yaml
  run touchdisp-posted-side-effect  scenarios/touchdisp-posted-side-effect.yaml
  ;;
proc)
  run mproc3-two-process            scenarios/mproc3-two-process.yaml
  run mproc8-bind-only              scenarios/mproc8-bind-only.yaml
  run binderpid                     scenarios/binderpid.yaml
  run launch-extras                 scenarios/launch-extras.yaml
  run launch-extras-parcelable      scenarios/launch-extras-parcelable.yaml
  ;;
web)
  for rep in 1 2 3; do
    run "webview-probe-$rep"        scenarios/webview-probe.yaml
  done
  for rep in 1 2 3; do
    run "webview-fair-$rep"         scenarios/webview-fair.yaml
  done
  ;;
sysui)
  run sysui1-permission-allow       scenarios/sysui1-permission-allow.yaml
  run sysui1-permission-deny        scenarios/sysui1-permission-deny.yaml
  run sysui1-permission-pregranted  scenarios/sysui1-permission-pregranted.yaml
  run sysui1-shade                  scenarios/sysui1-shade.yaml
  run notif-granted                 scenarios/notif-perm-granted.yaml
  run notif-denied                  scenarios/notif-perm-denied.yaml
  ;;
devices)
  run location-attach               results/handoff/MODLOC3/location-live-attach.yaml
  run location-geocoder             results/handoff/MODLOC2/location-geocoder.yaml
  run location-detach               results/handoff/MODLOC2/location-live-detach.yaml
  run sensors-scripted              scenarios/sensors-scripted.yaml
  run sensors-off                   scenarios/sensors-off.yaml
  ;;
*)
  echo "unknown group $group"; exit 2 ;;
esac
echo "SCENARIO_FAILURES $fail ($group)"
exit $fail
