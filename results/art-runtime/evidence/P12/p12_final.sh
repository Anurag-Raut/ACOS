#!/usr/bin/env bash
# P12 final validation: core gates in both animation modes, counter A/B (n=10),
# and the progress/background/delay probes. Real ART, release binary.
set -u
ACOS="$1"; ART="$2"; ROOT="$3"
export ACOS_ART_BIN="$ART"
mkdir -p "$ROOT"
run() { # name scenario anim
  local name="$1"
  local s="$2"
  local anim="$3"
  local d="$ROOT/$name"
  mkdir -p "$d"
  local apk
  apk=$(grep -m1 '^app:' "$s" | sed 's/^app:[[:space:]]*//')
  "$ACOS" verify --scenario "$s" --apk "$apk" --engine art --animations "$anim" \
      --artifacts "$d" --json >"$d/out.json" 2>"$d/err.log"
  local rc=$?
  python3 - "$d" "$name" "$anim" "$rc" <<'PY'
import json,sys,os
d,name,anim,rc=sys.argv[1],sys.argv[2],sys.argv[3],int(sys.argv[4])
try: out=json.load(open(os.path.join(d,'out.json')))
except Exception as e: print(name,anim,'ERROR',e); sys.exit()
v=(out.get('result') or {}).get('verdict') or {}
steps=v.get('steps') or []
passed=sum(1 for s in steps if s.get('status')=='pass'); total=len(steps)
print('%-26s anim=%-3s rc=%d %s %d/%d' % (name,anim,rc,v.get('verdict'),passed,total))
PY
}
# core gates, both modes (T03-wrong is the expected ASSERTION_FAILED control)
for anim in on off; do
  run "counter-$anim" scenarios/v2-art-counter.yaml $anim
  run "w22-$anim" scenarios/w22-counter-reset.yaml $anim
  run "w54-$anim" scenarios/w54-dialog-pixels.yaml $anim
  run "w42-$anim" scenarios/w42-back-frame.yaml $anim
  run "w15-$anim" scenarios/w15-dialog-menu.yaml $anim
  run "form-$anim" scenarios/form-validation.yaml $anim
  run "T03good-$anim" scenarios/suite/T03-render-good.yaml $anim
  run "T03wrong-$anim" scenarios/suite/T03-render-wrong.yaml $anim
done
# progress with animations off; P11 negative control with animations on
run progress-off scenarios/p12-progress-anim-off.yaml off
run progress-timeout-on scenarios/p11-progress-timeout.yaml on
run background-off scenarios/p11-background.yaml off
run background-on scenarios/p11-background.yaml on
run delay1000-off scenarios/p11-delay1000.yaml off
run delay1000-on scenarios/p11-delay1000.yaml on
# counter A/B n=10: three taps per run (steps 4, 7, 9)
for anim in on off; do
  for i in $(seq 1 10); do
    run "ab-$anim-$i" scenarios/v2-art-counter.yaml $anim
  done
done
