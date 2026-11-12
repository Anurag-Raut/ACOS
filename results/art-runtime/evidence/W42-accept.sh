#!/usr/bin/env bash
# W42 acceptance rows on the shared `target/debug` binaries.
#
# Each row is one `acos verify <scenario> --engine art --json` under the
# exclusive ART lock; the verdict and the first failing step are printed one per
# line, TSV, for results/art-runtime/evidence/W42-shared.tsv.
#
# W42 rows: the packet's list (counter v2, w19, w23, two-activity, dialog-menu,
# w27-dialog-window, w28-gaten-release-tap, w38-multi-activity) plus the new
# strict `w42-back-frame` (region hashes after Back).
set -uo pipefail
ROOT=/home/Qui/Projects/acos
cd "$ROOT" || exit 1
OUT=${1:-.cache/w42-accept}
mkdir -p "$OUT"
rows=(
  "v2-counter:scenarios/v2-art-counter.yaml"
  "w19-form-behavior:scenarios/w19-form-behavior.yaml"
  "w23-rn-tap:scenarios/w23-rn-hello-tap.yaml"
  "two-activity:scenarios/w15-two-activity.yaml"
  "dialog-menu:scenarios/w15-dialog-menu.yaml"
  "w27-dialog-window:scenarios/w27-dialog-window.yaml"
  "w28-gaten-release:scenarios/w28-gaten-release-tap.yaml"
  "w38-multi-activity:scenarios/w38-multi-activity.yaml"
  "w42-back-frame:scenarios/w42-back-frame.yaml"
)
: > "$OUT/shared.tsv"
i=0
for row in "${rows[@]}"; do
  i=$((i+1))
  name=${row%%:*}
  scenario=${row#*:}
  # short directory: the launcher's socket lives under it and the
  # unix-socket path limit is ~107 bytes.
  art="$OUT/r$i"
  rm -rf "$art"
  s=$(date -u +%FT%TZ)
  # short task name: run-exclusive.sh derives ACOS_ART_DATA from it and the
  # launcher's unix socket path is limited to ~107 bytes.
  tools/art-runtime/run-exclusive.sh --task "W42a$i" -- \
    tools/guarded.sh --mem 8G --time 900 -- \
    env ACOS_ART_BIN=$ROOT/target/debug/acos-art \
    "$ROOT/target/debug/acos" verify "$scenario" --engine art --json \
      --artifacts "$art" > "$OUT/$name.json" 2> "$OUT/$name.err"
  rc=$?
  python3 - "$art/verdict.json" "$name" "$scenario" "$rc" "$s" >> "$OUT/shared.tsv" <<'PY'
import json, sys
path, name, scenario, rc, start = sys.argv[1:6]
try:
    d = json.load(open(path))
    steps = d.get("steps") or []
    verdict = d.get("verdict")
    failed = next((f"{s['index']}:{s['step']}:{s.get('code','')}" for s in steps
                   if s.get("status") not in ("pass", "unsupported")), "-")
    wall = (d.get("metrics") or {}).get("wall_ms")
    print(f"{name}\t{verdict}\t{scenario}\trc={rc}\tstart={start}\twall_ms={wall}\tfailed={failed}")
except Exception as e:  # noqa: BLE001 - a missing verdict is a failure row
    print(f"{name}\tmissing-verdict\t{scenario}\trc={rc}\tstart={start}\terror={e}")
PY
  echo "DONE $name $(date -u +%FT%TZ)" >&2
done
