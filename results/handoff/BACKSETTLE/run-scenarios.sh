#!/usr/bin/env bash
# BACKSETTLE correctness: the three re-pointed scenarios plus every settle /
# Back / dispatch scenario the rule can touch, on real ART, with the backsettle
# release binary. One shared slot per run (correctness only: no timing numbers
# are claimed from these).
#
# Usage: run-scenarios.sh [name...]   (default: all of §4 of README.md)
set -uo pipefail
WT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
cd "$WT" || exit 2
export ACOS_FONT_DIR=/home/Qui/Projects/acos/.cache/boot/android-34/fonts
export ACOS_RUNTIME_DIR="$WT/.cache/rt-backsettle"
export ACOS_SESSION_SOCKET="$WT/.cache/rt-backsettle/session.sock"
export ACOS_VIEW_TEST_DIR="$WT/.cache/lv"
unset ACOS_BOOT_DIR XDG_RUNTIME_DIR ACOS_HOME
ACOS="$WT/.cache/backsettle-target/release/acos"
OUT="$WT/results/handoff/BACKSETTLE/runs"
mkdir -p "$OUT" "$ACOS_RUNTIME_DIR" "$ACOS_VIEW_TEST_DIR"

run() { # name yaml
    local name="$1" yaml="$2"
    local dir="$OUT/$name"
    rm -rf "$dir"
    echo "=== $(date -u +%FT%TZ) $name ($yaml)"
    "$WT/tools/art-runtime/run-exclusive.sh" --task BACKSETTLE -- \
        "$WT/tools/guarded.sh" --mem 12G --time 900 -- \
        env ACOS_ART_BIN="$WT/.cache/backsettle-target/release/acos-art" \
        "$ACOS" verify "$yaml" --engine art --artifacts "$dir" \
        > "$OUT/$name.log" 2> "$OUT/$name.err"
    echo "=== $name rc=$?"
    tail -3 "$OUT/$name.log"
    python3 - "$dir" "$name" "$yaml" <<'PY'
import json, sys, pathlib
try:
    import yaml
except Exception:
    yaml = None
d = pathlib.Path(sys.argv[1]); name = sys.argv[2]; scen = pathlib.Path(sys.argv[3])
v = d / "verdict.json"
if not v.exists():
    print(f"   {name}: NO verdict.json"); raise SystemExit
doc = json.loads(v.read_text())
decl = {}
if yaml is not None:
    decl = yaml.safe_load(scen.read_text()) or {}
steps = doc.get("steps") or []
bad = [(s.get("index"), s.get("kind"), s.get("status"),
        ((s.get("error") or {}).get("code")))
       for s in steps if s.get("status") not in ("pass", "skipped")]
err = doc.get("error") or {}
got = ("pass" if doc.get("status") == "pass"
       else "fail" if doc.get("status") == "fail"
       else "non_pass")
want = decl.get("expectation")
ok = got == want
print("   %-32s declared=%-9s got=%-9s steps=%d/%d code=%s reason=%s %s %s" % (
    name, want, got,
    sum(1 for s in steps if s.get("status") == "pass"), len(steps),
    err.get("code"), err.get("reason"),
    "MATCH" if ok else "MISMATCH", "" if not bad else f"non-pass={bad}"))
PY
}

if [ $# -gt 0 ]; then
    for name in "$@"; do
        case "$name" in
            hello-views) run hello-views scenarios/hello-views.yaml ;;
            multi-activity) run multi-activity scenarios/multi-activity.yaml ;;
            multi-wrong-after-back) run multi-wrong-after-back scenarios/negative/multi-wrong-after-back.yaml ;;
            *) run "$name" "scenarios/$name.yaml" ;;
        esac
    done
    exit 0
fi

run hello-views scenarios/hello-views.yaml
run multi-activity scenarios/multi-activity.yaml
run multi-wrong-after-back scenarios/negative/multi-wrong-after-back.yaml
run back-layers scenarios/back-layers.yaml
run back-layers-ime scenarios/back-layers-ime.yaml
run gaten-backstack scenarios/gaten-backstack.yaml
run rel3h-unhandled-input scenarios/rel3h-unhandled-input.yaml
run rel3h-unhandled-input-expect-change scenarios/rel3h-unhandled-input-expect-change.yaml
run rel3h2-expect-handled-false scenarios/rel3h2-expect-handled-false.yaml
run rel3h2-expect-handled-true scenarios/rel3h2-expect-handled-true.yaml
run touchdisp-dispatch-probe scenarios/touchdisp-dispatch-probe.yaml
run touchdisp-gesture scenarios/touchdisp-gesture.yaml
run touchdisp-posted-side-effect scenarios/touchdisp-posted-side-effect.yaml
run form-validation scenarios/form-validation.yaml
run mproc3-two-process scenarios/mproc3-two-process.yaml

echo "=== SCENARIOS DONE $(date -u +%FT%TZ)"
