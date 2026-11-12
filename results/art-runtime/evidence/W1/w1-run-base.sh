#!/usr/bin/env bash
set -u
WT=/home/Qui/.paseo/worktrees/3oxa6xao/perf-w1-capture-mem
cd "$WT" || exit 2
export ACOS_FONT_DIR=/home/Qui/Projects/acos/.cache/boot/android-34/fonts
export ACOS_ART_BIN=/home/Qui/Projects/acos/.cache/w1-base-target/release/acos-art
B=/home/Qui/Projects/acos/.cache/w1-base
python3 research/zm2/part1.py --mode normal --n 1 --out "$B/n1" --json "$B/n1.json" || exit 1
python3 research/zm2/part1.py --mode normal --n 5 --out "$B/n5" --json "$B/n5.json" || exit 1
python3 results/art-runtime/evidence/D-M5/probe.py --acos /home/Qui/Projects/acos/.cache/w1-base-target/release/acos \
  --outdir "$B/dm5" --samples 22 --warmup 2 --json "$B/dm5.json" || exit 1
echo BASE-DONE
