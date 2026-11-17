#!/usr/bin/env bash
# A11YPACE: the acceptance batch. One session per scenario, every ART run through
# the shared lock + guard, a private runtime dir, and the policy-build's release
# binaries. Usage: results/handoff/A11YPACE/run-regression.sh [scenario...]
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
cd "$ROOT"
export ACOS_FONT_DIR=/home/Qui/Projects/acos/.cache/boot/android-34/fonts
export ACOS_RUNTIME_DIR="$ROOT/.cache/a11ypace-run"
export ACOS_SESSION_SOCKET="$ROOT/.cache/a11ypace-run/session.sock"
export ACOS_VIEW_TEST_DIR="$ROOT/.cache/lv"
export ACOS_ART_BIN="$ROOT/.cache/a11ypace-target/release/acos-art"
unset ACOS_ART_SYSROOT ACOS_HOME || true
scenarios=("$@")
if [ ${#scenarios[@]} -eq 0 ]; then
  scenarios=(
    scenarios/a11y-probe.yaml
    scenarios/a11y-screenshot-wrap.yaml
    scenarios/rel3s-gaten-scroll.yaml
    scenarios/back-layers.yaml
    scenarios/gaten-bubble-overlay.yaml
  )
fi
for scenario in "${scenarios[@]}"; do
  name="$(basename "$scenario" .yaml)"
  out="$ROOT/results/handoff/A11YPACE/regression/$name"
  rm -rf "$out"
  echo "=== $name"
  # The host's own trace (ACOSA11Y dispatchGesture pacing, ACOSINPUT targets) is
  # evidence for the gesture work; the run's artifacts hold the guest half.
  ACOS_TRACE_FILE="$ROOT/results/handoff/A11YPACE/regression/$name-host-trace.log" \
  tools/art-runtime/run-exclusive.sh --task A11YPACE -- \
    tools/guarded.sh --mem 10G --time 600 -- \
    env -u ACOS_HOME "$ROOT/.cache/a11ypace-target/release/acos" verify \
      --engine art --scenario "$scenario" --json --artifacts "$out" \
      >"$out.log" 2>&1 || true
  python3 - "$out/verdict.json" "$name" <<'PY'
import json, sys
path, name = sys.argv[1], sys.argv[2]
try:
    v = json.load(open(path))
except Exception as error:
    print(f"{name}: no verdict ({error})")
    raise SystemExit(0)
steps = v.get("steps", [])
passed = sum(1 for s in steps if s.get("status") == "pass")
print(f"{name}: {v.get('status')} {passed}/{len(steps)} steps")
for s in steps:
    if s.get("status") != "pass":
        print(f"  {s.get('index')} {s.get('step')} {s.get('status')}: "
              f"{(s.get('error') or {}).get('message', '')[:140]}")
PY
done
