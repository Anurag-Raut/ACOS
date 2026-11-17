#!/usr/bin/env bash
# PERF4 indicative A/B (v0.2.2 vs perf4), SHARED slot: 5 reps, fast loop.
#
# The release gate's exclusive A/B is the bench owner's; this run is indicative
# (one shared slot, other agents' ART runs may share the host). Per rep the two
# arms are interleaved (A latency, B latency) and every acquisition goes through
# run-exclusive.sh --task PERF4 (no --exclusive) + tools/guarded.sh, with the
# ADR-0028 sample counts (20 strict / 40 fast per rep per metric).
#
# Usage: REPS=5 run-ab-shared.sh
set -uo pipefail
WT="/home/Qui/.t3/worktrees/acos/t3-5cf7912d-cec9a432baa8a011"
cd "$WT" || exit 2
export ACOS_FONT_DIR=/home/Qui/Projects/acos/.cache/boot/android-34/fonts
unset ACOS_BOOT_DIR XDG_RUNTIME_DIR ACOS_HOME
export ACOS_BENCH_RUN_ID="perf4-ab"
export ACOS_BENCH_PIDFILE="$WT/.cache/p4/perf4-ab.pids"
: > "$ACOS_BENCH_PIDFILE"
. "$WT/tools/bench/reap-daemons.sh"
trap 'bench_reap_daemons' EXIT INT TERM

BENCH="$WT/tools/bench/acos_bench.py"
OUT="${AB_OUT:-$WT/results/handoff/PERF4/03-ab}"
SCRATCH_ROOT="$WT/.cache/p4b"
declare -A BIN=([v022]="$WT/.cache/p4/v022-target/release/acos" \
                [fix]="$WT/.cache/p4-target/release/acos")
REPS="${REPS:-5}"
mkdir -p "$OUT" "$SCRATCH_ROOT"

for arm in v022 fix; do
    printf 'arm %s binary %s sha256 %s\n' "$arm" "${BIN[$arm]}" \
        "$(sha256sum "${BIN[$arm]}" | cut -d' ' -f1)" >> "$OUT/arms.txt"
done
"${BIN[v022]}" --version >> "$OUT/arms.txt"
"${BIN[fix]}" --version >> "$OUT/arms.txt"

for rep in $(seq 1 "$REPS"); do
    for arm in v022 fix; do
        phases="latency"; mem="8G"; tmax="540"
        outdir="$OUT/run-$arm/rep-$rep"
        mkdir -p "$outdir"
        scratch="$SCRATCH_ROOT/r$rep-$arm"
        rm -rf "$scratch"; mkdir -p "$scratch"
        echo "=== $(date -u +%FT%TZ) rep=$rep arm=$arm mem=$mem load1=$(cut -d' ' -f1 /proc/loadavg)"
        "$WT/tools/art-runtime/run-exclusive.sh" --task PERF4 -- \
            "$WT/tools/guarded.sh" --mem "$mem" --time "$tmax" -- \
            python3 "$BENCH" --bundle "$phases" --acos "${BIN[$arm]}" \
            --out-dir "$outdir" --scratch-root "$scratch" \
            --reps 20 --fast-reps 40 --p11-reps 3 --loops 0 --animations on
        rc=$?
        echo "=== $(date -u +%FT%TZ) rep=$rep arm=$arm rc=$rc load1=$(cut -d' ' -f1 /proc/loadavg)"
        rm -rf "$scratch"
    done
done

for arm in v022 fix; do
    python3 "$BENCH" --merge --run-dir "$OUT/run-$arm" --acos "${BIN[$arm]}" \
        --out "$OUT/$arm.json" --markdown "$OUT/$arm.md" --mode full
    echo "merge $arm rc=$?"
done

python3 "$WT/tools/bench/ab-compare.py" --a "$OUT/v022.json" --b "$OUT/fix.json" \
    --label-a "v0.2.2 (tag)" --label-b "perf4" \
    --json "$OUT/ab-compare.json" 2>&1 | tee "$OUT/ab-compare.txt" | tail -40

echo "=== DRIVER DONE $(date -u +%FT%TZ)"
