#!/usr/bin/env bash
# Z7 lifetime matrix: $1 = repetition, $2 = N. One arm per invocation of the
# harness; the arm order is rotated by repetition so host drift is interleaved.
set -u
REP="$1"; N="$2"
cd /home/Qui/.paseo/worktrees/3oxa6xao/zygote-z7
export ACOS_FONT_DIR=/home/Qui/Projects/acos/.cache/boot/android-34/fonts
export ACOS_ART_BIN=/home/Qui/Projects/acos/.cache/z7-target/release/acos-art
CACHE=/home/Qui/Projects/acos/.cache
case "$REP" in
  1) ORDER="a ap c cp" ;;
  2) ORDER="cp c ap a" ;;
  *) ORDER="c a cp ap" ;;
esac
for arm in $ORDER; do
  out="$CACHE/z7-runs/lifetime/rep$REP-n$N/$arm"
  json="$CACHE/z7-runs/lifetime/rep$REP-n$N/$arm.json"
  python3 research/zygote-proto/z7/lifetime.py --arm "$arm" --n "$N" \
    --out "$out" --json "$json" --label "rep$REP-n$N" --ages 0.5,2,5,15,60 || true
  # Screenshot PNGs are not evidence for a memory number; remove them per run.
  find "$out" -type d -name live-frames -prune -exec rm -rf {} + 2>/dev/null || true
done
