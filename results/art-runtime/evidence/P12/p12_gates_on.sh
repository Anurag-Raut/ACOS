#!/usr/bin/env bash
# P12 final gates after the review rebase: event mode. The animation mode is
# per-case: functional gates run with animations on (P11 default) except the
# progress-off counterpart. p11-progress-timeout is the negative control: its
# tap's strict settle times out, so verify must fail it as TIMEOUT (non-pass).
set -u
ACOS="$1"; ROOT="$2"; ART="$3"
export ACOS_ART_BIN="$ART"
run() {
  local name="$1"
  local scenario="$2"
  local expect="$3"
  local anim="$4"
  local d="$ROOT/$name"
  local apk rc verdict code
  mkdir -p "$d"
  apk=$(grep -m1 '^app:' "$scenario" | sed 's/^app:[[:space:]]*//')
  "$ACOS" verify --scenario "$scenario" --apk "$apk" --engine art --animations "$anim" \
      --artifacts "$d" --json >"$d/out.json" 2>"$d/err.log"
  rc=$?
  read -r verdict code < <(python3 - "$d/out.json" <<'PY'
import json,sys
try: d=json.load(open(sys.argv[1]))
except Exception: print('ERROR -'); raise SystemExit
v=(d.get('result') or {}).get('verdict') or {}
f=v.get('failure') or {}
print(v.get('verdict'), (f.get('error') or {}).get('code'))
PY
)
  case "$expect" in
    pass) [ "$rc" = 0 ] && [ "$verdict" = pass ] && echo "PASS $name (anim=$anim)" || echo "FAIL $name anim=$anim rc=$rc verdict=$verdict code=$code" ;;
    fail) [ "$rc" != 0 ] && [ "$verdict" = fail ] && [ "$code" = ASSERTION_FAILED ] && echo "PASS $name (expected assertion failure)" || echo "FAIL $name anim=$anim rc=$rc verdict=$verdict code=$code" ;;
    non_pass) [ "$rc" != 0 ] && [ "$verdict" = timeout ] && [ "$code" = TIMEOUT ] && echo "PASS $name (expected non-pass: timeout/TIMEOUT)" || echo "FAIL $name anim=$anim rc=$rc verdict=$verdict code=$code" ;;
  esac
}
run counter scenarios/v2-art-counter.yaml pass on
run w22 scenarios/w22-counter-reset.yaml pass on
run w54 scenarios/w54-dialog-pixels.yaml pass on
run w42 scenarios/w42-back-frame.yaml pass on
run w15 scenarios/w15-dialog-menu.yaml pass on
run form scenarios/form-validation.yaml pass on
run T03good scenarios/suite/T03-render-good.yaml pass on
run T03wrong scenarios/suite/T03-render-wrong.yaml fail on
run progress-off scenarios/p12-progress-anim-off.yaml pass off
run progress-timeout scenarios/p11-progress-timeout.yaml non_pass on
