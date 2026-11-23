#!/usr/bin/env bash
# PROCUID: run the acceptance scenarios on the real ART engine and record one
# JSON line per run (verdict + artifact dir). Sequential, each run in its own
# fresh guarded scope; the ART lock is acquired per run so no other agent is
# blocked beyond one scenario.
set -uo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/../.."

OUT="${1:-results/handoff/PROCUID/scenarios}"
mkdir -p "$OUT"
ACOS=.cache/procuid-target/release/acos
export ACOS_FONT_DIR=/home/Qui/Projects/acos/.cache/boot/android-34/fonts
export ACOS_VIEW_TEST_DIR="$PWD/.cache/lv"
export ACOS_RUNTIME_DIR="$PWD/.cache/run"
export ACOS_SESSION_SOCKET="$PWD/.cache/session.sock"
export ACOS_ART_BIN="${ACOS_ART_BIN:-$PWD/.cache/procuid-target/release/acos-art}"

: > "$OUT/runs.tsv"
printf 'run_id\tscenario\tverdict\trc\twall_ms\n' >> "$OUT/runs.tsv"

# PROCUID_ONLY="04 05" limits the run to matching ids (a failed suite row is
# re-run on its own, never by weakening it).
run() {
    local id="$1" scenario="$2"
    if [ -n "${PROCUID_ONLY:-}" ] && [[ " $PROCUID_ONLY " != *" ${id%%-*} "* ]]; then
        return
    fi
    local dir="$OUT/$id"
    rm -rf "$dir"
    local start end rc verdict wall
    start=$(date +%s%3N)
    tools/art-runtime/run-exclusive.sh --task PROCUID -- \
        tools/guard/guarded.sh --mem 8G --time 500 -- \
        env -u ACOS_HOME "$ACOS" verify "$scenario" --engine art --artifacts "$dir" \
        > "$OUT/$id.console.txt" 2>&1
    rc=$?
    end=$(date +%s%3N)
    wall=$((end - start))
    verdict=$(python3 - "$dir/verdict.json" <<'PY'
import json,sys
try:
    d=json.load(open(sys.argv[1]))
    print(d.get("verdict") or ("ok" if d.get("status")=="pass" else "fail"))
except Exception:
    print("no-verdict")
PY
)
    printf '%s\t%s\t%s\t%s\t%s\n' "$id" "$scenario" "$verdict" "$rc" "$wall" >> "$OUT/runs.tsv"
    echo "$id: $verdict (rc=$rc, ${wall}ms)"
}

run 01-binderpid scenarios/binderpid.yaml
run 02-mproc8-bind-only scenarios/mproc8-bind-only.yaml
run 03-mproc3-two-process scenarios/mproc3-two-process.yaml
run 04-notif-perm-granted scenarios/notif-perm-granted.yaml
run 05-notif-perm-denied scenarios/notif-perm-denied.yaml
run 06-a11y-probe scenarios/a11y-probe.yaml
run 07-location-pending-intent scenarios/location-pending-intent.yaml
run 08-counter scenarios/counter.yaml
run 09-gaten-backstack scenarios/gaten-backstack.yaml
