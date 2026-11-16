#!/usr/bin/env bash
# BENCHFIX: interleaved A/B driver (v0.2.2 tag vs rel-030 HEAD) with the fixed bench.
# One lock acquisition per (arm, rep, group). Scratch is removed after each phase.
set -uo pipefail
WT="/home/Qui/.t3/worktrees/acos/t3-0065c42b-364d4668d4838043"
cd "$WT" || exit 2
export ACOS_FONT_DIR=/home/Qui/Projects/acos/.cache/boot/android-34/fonts
unset ACOS_BOOT_DIR
export ACOS_BENCH_RUN_ID="benchfix-ab"
export ACOS_BENCH_PIDFILE="$WT/.cache/benchfix-ab.pids"
: > "$ACOS_BENCH_PIDFILE"
. "$WT/tools/bench/reap-daemons.sh"
trap 'bench_reap_daemons' EXIT INT TERM
BENCH="$WT/tools/bench/acos_bench.py"
OUT="$WT/results/bench/BENCHFIX-AB"
declare -A BIN=([v022]="$WT/.cache/v022-bin/acos" [rel030]="$WT/.cache/rel030-bin/acos")
REPS="${REPS:-3}"
for rep in $(seq 1 "$REPS"); do
    for arm in v022 rel030; do
        for grp in "latency,cold:8G:540" "memory:16G:600"; do
            phases="${grp%%:*}"; rest="${grp#*:}"
            mem="${rest%%:*}"; tmax="${rest##*:}"
            outdir="$OUT/run-$arm/rep-$rep"
            mkdir -p "$outdir"
            scratch="/home/Qui/Projects/acos/.cache/h6/benchfix-ab-r$rep-$arm-${phases%%,*}"
            rm -rf "$scratch"; mkdir -p "$scratch"
            echo "=== $(date -u +%FT%TZ) rep=$rep arm=$arm phases=$phases mem=$mem"
            "$WT/tools/art-runtime/run-exclusive.sh" --task BENCHFIX --exclusive -- \
                "$WT/tools/guarded.sh" --mem "$mem" --time "$tmax" -- \
                python3 "$BENCH" --bundle "$phases" --acos "${BIN[$arm]}" \
                --out-dir "$outdir" --scratch-root "$scratch" \
                --reps 20 --fast-reps 40 --p11-reps 3 --loops 0 --animations on
            rc=$?
            echo "=== $(date -u +%FT%TZ) rep=$rep arm=$arm phases=$phases rc=$rc"
            rm -rf "$scratch"
        done
    done
done
echo "=== DRIVER DONE $(date -u +%FT%TZ)"
