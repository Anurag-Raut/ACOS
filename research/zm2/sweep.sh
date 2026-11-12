#!/usr/bin/env bash
# ZM2 quick-win sweep: one N=1 session per allocator configuration, then N=5 for
# the best-looking one. Each run is a real ART session through part1.py.
set -u
OUT=/home/Qui/Projects/acos/.cache/zm2
export ACOS_ART_BIN=/home/Qui/Projects/acos/.cache/zm2c-target/release/acos-art
export ACOS_FONT_DIR=/home/Qui/Projects/acos/.cache/boot/android-34/fonts
cd "$(dirname "$0")/../.." || exit 2

run1() {
  name=$1; shift
  echo "== $name"
  env "$@" python3 research/zm2/part1.py --mode normal --n 1 \
    --out "$OUT/$name" --json "$OUT/$name.json" || exit 1
}

run1 n1-mmap128     ACOS_ART_MALLOC_MMAP_THRESHOLD_KB=128
run1 n1-mmap128trim ACOS_ART_MALLOC_MMAP_THRESHOLD_KB=128 ACOS_ART_TRIM_AFTER_CAPTURE=1
run1 n1-arena1trim  ACOS_ART_MALLOC_ARENA_MAX=1 ACOS_ART_TRIM_AFTER_CAPTURE=1

echo "== n5-mmap128trim"
env ACOS_ART_MALLOC_MMAP_THRESHOLD_KB=128 ACOS_ART_TRIM_AFTER_CAPTURE=1 \
  python3 research/zm2/part1.py --mode normal --n 5 \
  --out "$OUT/n5-mmap128trim" --json "$OUT/n5-mmap128trim.json" || exit 1
