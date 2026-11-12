#!/usr/bin/env bash
# Z7 form-validation lifetime/correctness: $@ = arms. N = $ACOS_Z7_FORM_N (default 5).
set -u
cd /home/Qui/.paseo/worktrees/3oxa6xao/zygote-z7
export ACOS_FONT_DIR=/home/Qui/Projects/acos/.cache/boot/android-34/fonts
export ACOS_ART_BIN=/home/Qui/Projects/acos/.cache/z7-target/release/acos-art
CACHE=/home/Qui/Projects/acos/.cache
N="${ACOS_Z7_FORM_N:-5}"
for arm in "$@"; do
  out="$CACHE/z7-runs/form/n$N-$arm"
  json="$CACHE/z7-runs/form/n$N-$arm.json"
  python3 research/zygote-proto/z7/lifetime.py --arm "$arm" --n "$N" \
    --out "$out" --json "$json" --app form-validation --ages 0.5,2,5,15,60 || true
  find "$out" -type d -name live-frames -prune -exec rm -rf {} + 2>/dev/null || true
done
