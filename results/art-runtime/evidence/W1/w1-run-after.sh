#!/usr/bin/env bash
set -u
WT=/home/Qui/.paseo/worktrees/3oxa6xao/perf-w1-capture-mem
T=/home/Qui/Projects/acos/.cache/w1-target
cd "$WT" || exit 2
export ACOS_FONT_DIR=/home/Qui/Projects/acos/.cache/boot/android-34/fonts
export ACOS_ART_BIN=$T/release/acos-art
B=/home/Qui/Projects/acos/.cache/w1-after
mkdir -p "$B/logs"
python3 research/zm2/part1.py --mode normal --n 1 --out "$B/n1" --json "$B/n1.json" || exit 1
python3 research/zm2/part1.py --mode normal --n 5 --out "$B/n5" --json "$B/n5.json" || exit 1
python3 /home/Qui/Projects/acos/.cache/w1-probe.py --acos "$T/release/acos" \
  --outdir "$B/dm5" --samples 22 --warmup 2 --json "$B/dm5.json" || exit 1
for s in scenarios/v2-art-counter.yaml scenarios/w54-dialog-pixels.yaml scenarios/w42-back-frame.yaml scenarios/form-validation.yaml scenarios/suite/T03-render-good.yaml scenarios/suite/T03-render-wrong.yaml; do
  name=$(basename "$s" .yaml)
  "$T/release/acos" verify "$s" --engine art --artifacts "$B/artifacts/$name" >"$B/logs/$name.log" 2>&1
  echo "GATE $name rc=$?"
done
CARGO_TARGET_DIR=$T CARGO_BUILD_JOBS=4 cargo test --release -p acos-cli -p acos-art -p acos-services --offline >"$B/logs/tests.log" 2>&1
echo "TESTS rc=$?"
grep -cE "^test result: ok" "$B/logs/tests.log"; grep -E "FAILED|panicked" "$B/logs/tests.log" | head
echo AFTER-DONE
