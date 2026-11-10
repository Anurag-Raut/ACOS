#!/usr/bin/env bash
#
# parity recorded -- offline UI-tree parity for every fixture app.
#
# Compares the committed physical-device captures (fixtures/reference-device/
# <app>/snapshot-00-launch.json, API 35 / 400 dpi) with the API-34 ART
# reference (fixtures/reference/<app>/art-api34-x86_64/ui_initial.xml, 420 dpi)
# through `acos parity --recorded`. No phone, emulator or VM is required.
#
# The density/inset ratio is applied to the device bounds first, so a
# `bounds_differ_density` difference is expected and labelled; a text/class/id
# difference is a real regression in the comparator.
#
# Usage:
#   tools/parity/recorded.sh                 # writes results/parity/recorded.json
#   ACOS_BIN=target/debug/acos tools/parity/recorded.sh
set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
BIN="${ACOS_BIN:-$ROOT/target/release/acos}"
OUT="${ACOS_PARITY_OUT:-$ROOT/results/parity}"
APPS=(counter todo-list multi-activity form-validation)

if [ ! -x "$BIN" ]; then
    echo "parity-recorded: $BIN is not executable; build it first (cargo build -p acos-cli)" >&2
    exit 2
fi

mkdir -p "$OUT"
status=0
merged="[]"
for app in "${APPS[@]}"; do
    json="$OUT/$app.json"
    # Recorded mode compares the device capture with the ART reference, so
    # differences are the expected density/inset/chrome ones. A non-zero exit
    # is only an error when the output is a structured failure (`code`, no
    # `matches`), not when it is a difference report.
    "$BIN" parity --recorded "$app" --bounds-scale 1.05 --bounds-tolerance 24 --json >"$json" || true
    if grep -q '"code"' "$json" && ! grep -q '"matches"' "$json"; then
        echo "parity-recorded: $app FAILED to produce a report (see $json)" >&2
        status=1
        continue
    fi
    summary="$(python3 - "$json" <<'PYEOF'
import json, sys
d = json.load(open(sys.argv[1]))
kinds = {}
for diff in d.get("differences", []):
    kinds[diff["kind"]] = kinds.get(diff["kind"], 0) + 1
print(d.get("matches", 0), d.get("identical", False),
      ",".join(f"{k}={v}" for k, v in sorted(kinds.items())))
PYEOF
)"
    echo "parity-recorded: $app matches/identical/differences: $summary"
    merged="$(python3 - "$merged" "$json" "$app" <<'PYEOF'
import json, sys
merged, path, app = sys.argv[1], sys.argv[2], sys.argv[3]
data = json.loads(open(path).read())
data["app"] = app
print(json.dumps(json.loads(merged) + [data], indent=2))
PYEOF
)"
done

printf '%s\n' "$merged" >"$OUT/recorded.json"
echo "parity-recorded: wrote $OUT/recorded.json"
exit $status
