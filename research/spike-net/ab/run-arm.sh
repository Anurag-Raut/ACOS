#!/usr/bin/env bash
# One A/B arm, one rep: the SPIKE-NET item-7 gate measurement.
#   run-arm.sh <arm:on|off> <rep> <groups>
set -uo pipefail
WT=/home/Qui/.t3/worktrees/acos/t3-bd758e68-c5d2c8dcdcd2504d
ARM=$1; REP=$2; GSPEC=$3
cd "$WT"
export ACOS_FONT_DIR=/home/Qui/Projects/acos/.cache/boot/android-34/fonts
export ACOS_ART_BIN="$WT/.cache/sn3-target/release/acos-art"
unset ACOS_BOOT_DIR XDG_RUNTIME_DIR
if [ "$ARM" = on ]; then
    export ACOS_SPIKE_NET=1
    export ACOS_SPIKE_NET_PASTA="$WT/.cache/passt-build/src/pasta"
else
    unset ACOS_SPIKE_NET ACOS_SPIKE_NET_PASTA
fi
unset ACOS_SPIKE_NET_DNS_LOG
OUT="$WT/results/bench/spike-net-$ARM-$REP.json"
LOG="$WT/.cache/ab/$ARM-$REP.log"
tools/bench/acos-bench.sh \
    --no-build \
    --reps 1 \
    --latency-reps 10 \
    --fast-reps 16 \
    --task SPIKE-NET \
    --target-dir "$WT/.cache/sn3-target" \
    --scratch-root "$WT/.cache/bench-scratch" \
    --run-dir "$WT/results/bench/spike-net-run-$ARM-$REP" \
    --out "$OUT" \
    --markdown "$WT/.cache/ab/$ARM-$REP.md" \
    --groups "$GSPEC" > "$LOG" 2>&1
rc=$?
echo "arm=$ARM rep=$REP rc=$rc out=$OUT log=$LOG"
tail -3 "$LOG"
