#!/usr/bin/env bash
set -u
cd /home/Qui/.paseo/worktrees/3oxa6xao/perf-w1-capture-mem || exit 2
export ACOS_FONT_DIR=/home/Qui/Projects/acos/.cache/boot/android-34/fonts
B=/home/Qui/Projects/acos/.cache/w1-lat; mkdir -p "$B"
probe() { # tag bin
  ACOS_ART_BIN=$2/acos-art python3 /home/Qui/Projects/acos/.cache/w1-probe.py --acos "$2/acos" \
    --outdir "$B/$1" --samples 40 --warmup 5 --json "$B/$1.json" || exit 1
}
BASE=/home/Qui/Projects/acos/.cache/w1-base-target/release
AFT=/home/Qui/Projects/acos/.cache/w1-target/release
probe base-c "$BASE"
probe after-c "$AFT"
probe base-d "$BASE"
probe after-d "$AFT"
echo LAT-DONE
