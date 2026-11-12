#!/usr/bin/env bash
# P9 gates: every scenario is run through the real `acos verify --engine art`.
BIN=/home/Qui/Projects/acos/.cache/p9-target/release/acos
OUT=/home/Qui/Projects/acos/.cache/p9-gates
mkdir -p "$OUT"
cd "$(dirname "$0")" >/dev/null 2>&1 || true
cd /home/Qui/.paseo/worktrees/3oxa6xao/perf-p9-fast-launch || exit 2
for scen in scenarios/v2-art-counter.yaml scenarios/w22-counter-reset.yaml \
            scenarios/form-validation.yaml scenarios/w54-dialog-pixels.yaml \
            scenarios/w15-dialog-menu.yaml scenarios/w42-back-frame.yaml \
            scenarios/suite/T03-render-good.yaml scenarios/suite/T03-render-wrong.yaml; do
  tag=$(basename "$scen" .yaml)
  "$BIN" verify "$scen" --engine art --artifacts "$OUT/$tag" >"$OUT/$tag.log" 2>&1
  rc=$?
  echo "gate $tag rc=$rc"
  tail -3 "$OUT/$tag.log" | sed 's/^/    /'
done
