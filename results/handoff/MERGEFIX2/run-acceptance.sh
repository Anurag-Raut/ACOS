#!/bin/bash
# MERGEFIX2 acceptance corpus. One `acos verify --engine art --json` per
# scenario in a private runtime dir; each scenario gets its **own**
# run-exclusive acquisition so no single lock hold exceeds a few minutes.
#
#   results/handoff/MERGEFIX2/run-acceptance.sh <outdir> [name...]
#
# The per-scenario env (runtime dir, session socket, view dir, ACOS_ART_BIN) lives
# in run-one.sh so the scenarios here stay a plain list.
set -u
WT=$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)
out=${1:?usage: run-acceptance.sh <outdir> [name...]}
shift
cd "$WT" || exit 2
mkdir -p "$out"

declare -A SC=(
  [gaten-backstack]=scenarios/gaten-backstack.yaml
  [gaten-lesson]=scenarios/gaten-lesson.yaml
  [gaten-bubble]=scenarios/gaten-bubble.yaml
  [gaten-bubble-overlay]=scenarios/gaten-bubble-overlay.yaml
  [sysui1-permission-allow]=scenarios/sysui1-permission-allow.yaml
  [sysui1-permission-deny]=scenarios/sysui1-permission-deny.yaml
  [sysui1-permission-pregranted]=scenarios/sysui1-permission-pregranted.yaml
  [sysui1-shade]=scenarios/sysui1-shade.yaml
  [a11y-probe]=scenarios/a11y-probe.yaml
  [mproc3-two-process]=scenarios/mproc3-two-process.yaml
  [back-layers]=scenarios/back-layers.yaml
  [form-validation]=scenarios/form-validation.yaml
  [notif-perm-granted]=scenarios/notif-perm-granted.yaml
  [notif-perm-denied]=scenarios/notif-perm-denied.yaml
  [location-attach]=results/handoff/MODLOC3/location-live-attach.yaml
  [location-geocoder]=results/handoff/MODLOC2/location-geocoder.yaml
  [location-detach]=results/handoff/MODLOC2/location-live-detach.yaml
  [sensors-scripted]=scenarios/sensors-scripted.yaml
  [sensors-off]=scenarios/sensors-off.yaml
)
order=(gaten-backstack gaten-lesson gaten-bubble gaten-bubble-overlay
       sysui1-permission-allow sysui1-permission-deny sysui1-permission-pregranted
       sysui1-shade a11y-probe mproc3-two-process back-layers form-validation
       notif-perm-granted notif-perm-denied location-attach location-geocoder
       location-detach sensors-scripted sensors-off)
if [ $# -gt 0 ]; then order=("$@"); fi

fail=0
for name in "${order[@]}"; do
  scenario=${SC[$name]:-}
  [ -n "$scenario" ] || { echo "SCENARIO $name: unknown"; fail=$((fail+1)); continue; }
  tools/art-runtime/run-exclusive.sh --task MERGEFIX2 -- \
      tools/guarded.sh --mem 6G --time 300 -- \
      results/handoff/MERGEFIX2/run-one.sh "$scenario" "$out" || fail=$((fail+1))
done
echo "SCENARIO_FAILURES $fail"
exit $(( fail > 0 ? 1 : 0 ))
