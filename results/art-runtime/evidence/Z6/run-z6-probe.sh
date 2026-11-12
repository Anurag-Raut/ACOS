#!/usr/bin/env bash
# Z6 release-mechanism runs (one lock acquisition):
#   1. (c) N=5 with the 20 s 1 Hz release probe  — what drops, when, in which mapping
#   2. (a) N=5 with the same probe               — the exec'd guest under the same eyes
#   3. (d) N=5 = (a) + ACOS_ART_HEAP* control    — "would (a) release the same memory
#      if ART ran the same trim?" (existing W48 knobs, no code change)
#   4. (d) N=1 for the same control at the smallest N (cheap OOM/behaviour check)
# Every run also takes the 0.5/60 s whole-tree samples, so (d) can be compared with
# (a)/(c) at the same time points.
set -uo pipefail
D=$(cd "$(dirname "$0")" && pwd)
REPO=$(cd "$D/../../../.." && pwd)
cd "$REPO" || exit 2
CACHE=/home/Qui/Projects/acos/.cache
export ACOS_CACHE=$CACHE
export ACOS_ART_BIN=$CACHE/z6-target/release/acos-art
export ACOS_FONT_DIR=$CACHE/boot/android-34/fonts
OUT=${Z6_OUT:-$CACHE/z6-run}
mkdir -p "$OUT"

run() {   # run <arm> <mode> <n> <suffix> <probe-seconds>
  local arm=$1 mode=$2 n=$3 sfx=$4 probe=$5
  local used
  used=$(free -m | awk '/Mem:/{print int(100*$3/$2)}')
  free -m | awk 'NR==1 || /Mem:/'
  if [ "$used" -ge 90 ]; then
    echo "Z6: host RAM ${used}% >= 90%, refusing" >&2
    exit 3
  fi
  echo "=== arm=$arm mode=$mode n=$n sfx=$sfx probe=$probe host=${used}% $(date -u +%FT%TZ) ==="
  rm -rf "$OUT/$arm-$n$sfx"
  python3 research/zygote-proto/z4/drive.py --mode "$mode" --n "$n" --label "$arm" \
    --samples 0.5,60 --release-probe "$probe" \
    --out "$OUT/$arm-$n$sfx" --json "$OUT/$arm-$n$sfx.json"
  echo "=== arm=$arm n=$n$sfx rc=$? $(date -u +%FT%TZ) ==="
}

# 1/2: the probe on both real arms, 20 s of idle time right after the last first frame
export ACOS_ZYGOTE_NO_STUB=1
unset ACOS_SHARED_BOOT_IMAGE ACOS_ART_HEAPGROWTHLIMIT ACOS_ART_HEAPTARGETUTILIZATION
run c zygote 5 -probe 20
unset ACOS_ZYGOTE_NO_STUB
run a normal 5 -probe 20

# 3/4: the exec path pushed by heap policy alone (W48 seam), no zygote
export ACOS_ART_HEAPGROWTHLIMIT=${Z6_HEAPGROWTHLIMIT:-48m}
export ACOS_ART_HEAPTARGETUTILIZATION=${Z6_HEAPTARGETUTILIZATION:-0.9}
run d normal 1 -probe 0
run d normal 5 -probe 20
unset ACOS_ART_HEAPGROWTHLIMIT ACOS_ART_HEAPTARGETUTILIZATION
