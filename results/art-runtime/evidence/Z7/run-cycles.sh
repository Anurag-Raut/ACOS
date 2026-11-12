#!/usr/bin/env bash
# Z7 20-cycle teardown/leak test: $@ = arms. N=5, 20 fixed rounds per arm.
set -u
cd /home/Qui/.paseo/worktrees/3oxa6xao/zygote-z7
export ACOS_FONT_DIR=/home/Qui/Projects/acos/.cache/boot/android-34/fonts
export ACOS_ART_BIN=/home/Qui/Projects/acos/.cache/z7-target/release/acos-art
CACHE=/home/Qui/Projects/acos/.cache
for arm in "$@"; do
  out="$CACHE/z7-runs/cycles/n5-$arm"
  json="$CACHE/z7-runs/cycles/n5-$arm.json"
  python3 research/zygote-proto/z7/loop.py --arm "$arm" --n 5 --cycles 20 \
    --close-bound 5 --close-order owner-first --app counter \
    --out "$out" --json "$json" || true
  find "$out" -type d -name live-frames -prune -exec rm -rf {} + 2>/dev/null || true
done
