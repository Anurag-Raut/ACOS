#!/bin/sh
# MERGE040C acceptance corpus: every scenario the brief names, one ART
# acquisition per run (the shared lock is never held for the batch), evidence in
# results/handoff/MERGE040C/art/<name>/.
#
# usage: run-corpus.sh [name ...]     (default: the whole list)
set -u
cd "$(dirname "$0")/../../.." || exit 1
. .cache/env.sh
out=results/handoff/MERGE040C/art
mkdir -p "$out"
list=${*:-}
if [ -z "$list" ]; then
  list="scenarios/hello-views.yaml scenarios/multi-activity.yaml \
scenarios/negative/multi-wrong-after-back.yaml scenarios/back-layers.yaml \
scenarios/back-layers-ime.yaml scenarios/gaten-backstack.yaml \
scenarios/gaten-lesson.yaml scenarios/rel3h-unhandled-input.yaml \
scenarios/rel3h-unhandled-input-expect-change.yaml \
scenarios/rel3h2-expect-handled-true.yaml \
scenarios/rel3h2-expect-handled-false.yaml \
scenarios/touchdisp-dispatch-probe.yaml scenarios/touchdisp-gesture.yaml \
scenarios/touchdisp-posted-side-effect.yaml scenarios/form-validation.yaml \
scenarios/mproc3-two-process.yaml scenarios/sysui1-shade.yaml \
scenarios/a11y-probe.yaml scenarios/counter.yaml"
fi
for scen in $list; do
  name=$(basename "$scen" .yaml)
  dir=$out/$name
  rm -rf "$dir"
  start=$(date +%s)
  tools/art-runtime/run-exclusive.sh --task MERGE040C -- \
    tools/guarded.sh --mem 10G --time 400 -- \
    "$ACOS_BIN" verify "$scen" --engine art --artifacts "$dir" \
    > "$dir.log" 2>&1
  rc=$?
  end=$(date +%s)
  status=$(python3 - "$dir" <<'PY'
import json, sys
try:
    v = json.load(open(sys.argv[1] + "/verdict.json"))
    print("%s/%s" % (v.get("status"), (v.get("error") or {}).get("code") if isinstance(v.get("error"), dict) else v.get("error")))
except Exception:
    print("NO_VERDICT")
PY
)
  printf '%-42s rc=%s %-24s %ss\n' "$name" "$rc" "$status" "$((end - start))"
done
