#!/usr/bin/env bash
# P10 gate runner: real ART, release binary, one scenario per invocation.
# Usage: p10_gates.sh <acos-binary> <artifacts-root>
set -u
ACOS="$1"
ROOT="$2"
mkdir -p "$ROOT"
printf '%-24s %-8s %s\n' scenario verdict "pass/fail/total"
for s in scenarios/v2-art-counter.yaml scenarios/form-validation.yaml \
         scenarios/w54-dialog-pixels.yaml scenarios/w15-dialog-menu.yaml \
         scenarios/w42-back-frame.yaml scenarios/suite/T03-render-good.yaml \
         scenarios/suite/T03-render-wrong.yaml; do
  name=$(basename "$s" .yaml)
  apk=$(grep -m1 '^app:' "$s" | sed 's/^app:[[:space:]]*//')
  dir="$ROOT/$name"
  mkdir -p "$dir"
  "$ACOS" verify --scenario "$s" --apk "$apk" --engine art --artifacts "$dir" \
      --json >"$dir/out.json" 2>"$dir/err.log"
  rc=$?
  python3 - "$dir/out.json" "$name" "$rc" <<'PY'
import json, sys
path, name, rc = sys.argv[1], sys.argv[2], int(sys.argv[3])
try:
    d = json.load(open(path))
except Exception as exc:
    print("%-24s %-8s %s" % (name, "error", exc)); sys.exit(0)
v = ((d.get("result") or {}).get("verdict") or {})
steps = v.get("steps") or []
passed = sum(1 for s in steps if s.get("status") == "pass")
failed = sum(1 for s in steps if s.get("status") != "pass")
total = len(steps)
verdict = "pass" if (v.get("verdict") == "pass" and failed == 0 and total > 0 and rc == 0) else "fail"
failure = (d.get("error") or {})
code = failure.get("code") if isinstance(failure, dict) else (failure or "")
print("%-24s %-8s %d/%d/%d %s" % (name, verdict, passed, failed, total, code))
PY
done
