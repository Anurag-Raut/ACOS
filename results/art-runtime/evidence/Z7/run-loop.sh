#!/usr/bin/env bash
# Z7 agent loop: $@ = arms. N=10, >=5 min per arm, round = fresh launch per task.
set -u
cd /home/Qui/.paseo/worktrees/3oxa6xao/zygote-z7
export ACOS_FONT_DIR=/home/Qui/Projects/acos/.cache/boot/android-34/fonts
export ACOS_ART_BIN=/home/Qui/Projects/acos/.cache/z7-target/release/acos-art
CACHE=/home/Qui/Projects/acos/.cache
for arm in "$@"; do
  out="$CACHE/z7-runs/loop/n10-$arm"
  json="$CACHE/z7-runs/loop/n10-$arm.json"
  python3 research/zygote-proto/z7/loop.py --arm "$arm" --n 10 --minutes 5.2 \
    --close-bound 3 --close-order owner-first --app counter \
    --out "$out" --json "$json" || true
  find "$out" -type d -name live-frames -prune -exec rm -rf {} + 2>/dev/null || true
done
