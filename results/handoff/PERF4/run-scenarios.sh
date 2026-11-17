#!/usr/bin/env bash
# PERF4 correctness: the settle / handled / Back scenarios on real ART, with the
# perf4 release binary. One shared slot per acquisition (correctness runs: no
# timing numbers are claimed from these).
#
# Usage: run-scenarios.sh
set -uo pipefail
WT="/home/Qui/.t3/worktrees/acos/t3-5cf7912d-cec9a432baa8a011"
cd "$WT" || exit 2
export ACOS_FONT_DIR=/home/Qui/Projects/acos/.cache/boot/android-34/fonts
unset ACOS_BOOT_DIR XDG_RUNTIME_DIR ACOS_HOME
ACOS="$WT/.cache/p4-target/release/acos"
OUT="$WT/results/handoff/PERF4/04-scenarios"
mkdir -p "$OUT"

run() { # name yaml [extra args]
    local name="$1" yaml="$2"; shift 2
    local dir="$OUT/$name"
    rm -rf "$dir"
    echo "=== $(date -u +%FT%TZ) $name ($yaml)"
    "$WT/tools/art-runtime/run-exclusive.sh" --task PERF4 -- \
        "$WT/tools/guarded.sh" --mem 12G --time 600 -- \
        env ACOS_ART_BIN="$WT/.cache/p4-target/release/acos-art" \
        "$ACOS" verify "$yaml" --engine art --artifacts "$dir" "$@" \
        > "$OUT/$name.log" 2> "$OUT/$name.err"
    echo "=== $name rc=$?"
    tail -2 "$OUT/$name.log"
    python3 - "$dir" "$name" <<'PY'
import json, sys, pathlib
d = pathlib.Path(sys.argv[1])
v = d / "verdict.json"
if not v.exists():
    print("   NO verdict.json"); raise SystemExit
doc = json.loads(v.read_text())
steps = doc.get("steps") or []
bad = [(s.get("index"), s.get("kind"), s.get("status"),
        ((s.get("error") or {}).get("code")))
       for s in steps if s.get("status") not in ("pass", "skipped")]
print("   %-28s status=%-6s ok=%s steps=%d/%d %s" % (
    sys.argv[2], doc.get("status"), doc.get("ok"),
    sum(1 for s in steps if s.get("status") == "pass"), len(steps),
    "" if not bad else "NON-PASS: %s" % bad))
PY
}

run rel3h-unhandled-input scenarios/rel3h-unhandled-input.yaml
run rel3h-unhandled-input-expect-change scenarios/rel3h-unhandled-input-expect-change.yaml
run rel3h2-expect-handled-false scenarios/rel3h2-expect-handled-false.yaml
run rel3h2-expect-handled-true scenarios/rel3h2-expect-handled-true.yaml
run touchdisp-posted-side-effect scenarios/touchdisp-posted-side-effect.yaml
run touchdisp-gesture scenarios/touchdisp-gesture.yaml
run form-validation scenarios/form-validation.yaml
run back-layers scenarios/back-layers.yaml
run back-layers-ime scenarios/back-layers-ime.yaml

echo "=== SCENARIOS DONE $(date -u +%FT%TZ)"
