#!/usr/bin/env bash
# HYG2 acceptance scenarios on the real ART engine: the two notification gates,
# the PendingIntent location transport, the two-process scenario (whose new
# `creator` step is the PROCUID §7 ART probe) and the binder-identity scenario.
# One shared slot per run (correctness only: no timing numbers are claimed).
set -uo pipefail
WT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
cd "$WT" || exit 2
export ACOS_FONT_DIR=/home/Qui/Projects/acos/.cache/boot/android-34/fonts
export ACOS_RUNTIME_DIR="$WT/.cache/rt-hyg2"
export ACOS_SESSION_SOCKET="$WT/.cache/rt-hyg2/session.sock"
export ACOS_VIEW_TEST_DIR="$WT/.cache/lv"
export ACOS_ART_BIN="${ACOS_ART_BIN:-$WT/.cache/hyg2-target/debug/acos-art}"
unset ACOS_BOOT_DIR XDG_RUNTIME_DIR ACOS_HOME
ACOS="${ACOS:-$WT/.cache/hyg2-target/debug/acos}"
OUT="$WT/results/handoff/HYG2/runs"
mkdir -p "$OUT" "$ACOS_RUNTIME_DIR" "$ACOS_VIEW_TEST_DIR"
printf 'scenario\tverdict\trc\twall_ms\tartifacts\n' > "$OUT/runs.tsv"

run() { # name yaml
    local name="$1" yaml="$2"
    local dir="$OUT/$name"
    rm -rf "$dir"
    local start end rc verdict
    start=$(date +%s%3N)
    echo "=== $(date -u +%FT%TZ) $name ($yaml)"
    tools/art-runtime/run-exclusive.sh --task HYG2 -- \
        tools/guarded.sh --mem 12G --time 900 -- \
        "$ACOS" verify "$yaml" --engine art --artifacts "$dir" \
        > "$OUT/$name.log" 2> "$OUT/$name.err"
    rc=$?
    end=$(date +%s%3N)
    verdict=$(python3 - "$dir" <<'PY'
import json, pathlib, sys
v = pathlib.Path(sys.argv[1]) / "verdict.json"
print(json.loads(v.read_text()).get("verdict", "missing") if v.exists() else "missing")
PY
)
    printf '%s\t%s\t%s\t%s\t%s\n' "$name" "$verdict" "$rc" "$((end - start))" "$dir" >> "$OUT/runs.tsv"
    echo "=== $name verdict=$verdict rc=$rc"
    tail -2 "$OUT/$name.log"
}

run notif-perm-granted scenarios/notif-perm-granted.yaml
run notif-perm-denied scenarios/notif-perm-denied.yaml
run location-pending-intent scenarios/location-pending-intent.yaml
run mproc3-two-process scenarios/mproc3-two-process.yaml
run binderpid scenarios/binderpid.yaml
column -t -s $'\t' "$OUT/runs.tsv"
