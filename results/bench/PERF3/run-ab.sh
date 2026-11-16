#!/usr/bin/env bash
# PERF3: interleaved A/B driver (v0.2.2 tag vs the PERF3 fix) for the M8 form,
# M4 and M6 fast metrics. Same bench, flags and lock/guard discipline as
# results/bench/BENCHFIX-AB/run-ab.sh; only the `latency` bundle is run because
# those are the metrics PERF3 changes.
set -uo pipefail
WT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
cd "$WT" || exit 2
export ACOS_FONT_DIR=/home/Qui/Projects/acos/.cache/boot/android-34/fonts
unset ACOS_BOOT_DIR
export ACOS_BENCH_RUN_ID="perf3-ab"
export ACOS_BENCH_PIDFILE="$WT/.cache/perf3-ab.pids"
: > "$ACOS_BENCH_PIDFILE"
. "$WT/tools/bench/reap-daemons.sh"
trap 'bench_reap_daemons' EXIT INT TERM
BENCH="$WT/tools/bench/acos_bench.py"
OUT="$WT/results/bench/PERF3"
declare -A BIN=([v022]="$WT/.cache/arms/v022/acos" [fix]="$WT/.cache/arms/fix/acos")
REPS="${REPS:-3}"
for rep in $(seq 1 "$REPS"); do
    for arm in v022 fix; do
        outdir="$OUT/run-$arm/rep-$rep"
        mkdir -p "$outdir"
        scratch="$WT/.cache/perf3-ab-r$rep-$arm"
        rm -rf "$scratch"; mkdir -p "$scratch"
        echo "=== $(date -u +%FT%TZ) rep=$rep arm=$arm"
        "$WT/tools/art-runtime/run-exclusive.sh" --task PERF3 --exclusive -- \
            "$WT/tools/guarded.sh" --mem 8G --time 540 -- \
            python3 "$BENCH" --bundle latency --acos "${BIN[$arm]}" \
            --out-dir "$outdir" --scratch-root "$scratch" \
            --reps 20 --fast-reps 40 --p11-reps 3 --loops 0 --animations on
        rc=$?
        echo "=== $(date -u +%FT%TZ) rep=$rep arm=$arm rc=$rc"
        rm -rf "$scratch"
    done
done
echo "=== DRIVER DONE $(date -u +%FT%TZ)"
