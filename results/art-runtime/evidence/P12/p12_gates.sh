#!/usr/bin/env bash
# P12 gate runner: real ART, release binary, one scenario per invocation.
# Strict settle is event-only (P12); animations are the caller's choice:
#   p12_gates.sh <acos-binary> <artifacts-root> <on|off>
set -u
ACOS="$1"
ROOT="$2"
ANIM="$3"
mkdir -p "$ROOT"
printf 'animations=%s\n' "$ANIM"
printf '%-26s %-8s %s\n' scenario verdict "pass/fail/total"
for s in scenarios/v2-art-counter.yaml scenarios/w22-counter-reset.yaml \
         scenarios/w54-dialog-pixels.yaml scenarios/w42-back-frame.yaml \
         scenarios/w15-dialog-menu.yaml scenarios/form-validation.yaml \
         scenarios/suite/T03-render-good.yaml scenarios/suite/T03-render-wrong.yaml \
         scenarios/p11-anim.yaml scenarios/p11-delay40.yaml \
         scenarios/p11-delay200.yaml scenarios/p11-delay1000.yaml \
         scenarios/p11-background.yaml scenarios/p11-chain.yaml \
         scenarios/p11-dialog.yaml scenarios/p11-activity.yaml \
         scenarios/p11-list-smooth.yaml scenarios/p11-layout-transition.yaml \
         scenarios/p11-rapid-taps.yaml scenarios/p11-progress-timeout.yaml; do
  name=$(basename "$s" .yaml)
  apk=$(grep -m1 '^app:' "$s" | sed 's/^app:[[:space:]]*//')
  dir="$ROOT/$name"
  mkdir -p "$dir"
  "$ACOS" verify --scenario "$s" --apk "$apk" --engine art --animations "$ANIM" \
      --artifacts "$dir" --json >"$dir/out.json" 2>"$dir/err.log"
  rc=$?
  python3 - "$dir/out.json" "$name" "$rc" <<'PY'
import json, sys
path, name, rc = sys.argv[1], sys.argv[2], int(sys.argv[3])
try:
    d = json.load(open(path))
except Exception as exc:
    print("%-26s %-8s %s" % (name, "error", exc)); sys.exit(0)
v = ((d.get("result") or {}).get("verdict") or {})
steps = v.get("steps") or []
passed = sum(1 for s in steps if s.get("status") == "pass")
failed = sum(1 for s in steps if s.get("status") != "pass")
total = len(steps)
expectation = (v.get("scenario") or {}).get("expectation", "pass")
verdict = "pass" if (v.get("verdict") == "pass" and failed == 0 and total > 0 and rc == 0) else "fail"
failure = (d.get("error") or {})
code = failure.get("code") if isinstance(failure, dict) else (failure or "")
anim = v.get("animations")
print("%-26s %-8s %d/%d/%d expect=%s anim=%s %s" % (
    name, verdict, passed, failed, total, expectation, anim, code))
PY
done
