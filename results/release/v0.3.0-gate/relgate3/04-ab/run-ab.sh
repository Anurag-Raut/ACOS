#!/usr/bin/env bash
# RELGATE3 timing half: interleaved A/B (v0.2.2 tag vs v0.3.0 RC c7962f71) with the release
# bench, ADR-0028 lattice gate.
#
# Per rep: A latency+cold, A memory, B latency+cold, B memory -- one lock
# acquisition per group, every acquisition --exclusive (release-gate timing: no
# other ART run shares the host) and behind tools/guarded.sh, with the same
# group spec and sample counts tools/bench/acos-bench.sh uses for the full
# profile (latency 20 strict samples, fast 40 samples/rep per ADR-0028).
#
# Usage: REPS=3 run-ab.sh   (writes into this directory)
set -uo pipefail
WT="/home/Qui/.t3/worktrees/acos/t3-7a2f4ca5-3dab9ee7edba13ab"
cd "$WT" || exit 2
export ACOS_FONT_DIR=/home/Qui/Projects/acos/.cache/boot/android-34/fonts
unset ACOS_BOOT_DIR XDG_RUNTIME_DIR ACOS_HOME
export ACOS_BENCH_RUN_ID="relgate3-ab"
export ACOS_BENCH_PIDFILE="$WT/.cache/rg3/relgate3-ab.pids"
: > "$ACOS_BENCH_PIDFILE"
. "$WT/tools/bench/reap-daemons.sh"
trap 'bench_reap_daemons' EXIT INT TERM

BENCH="$WT/tools/bench/acos_bench.py"
OUT="$WT/results/release/v0.3.0-gate/relgate3/04-ab"
# Short scratch root on purpose: the daemon socket is <scratch>/serve.sock and
# sun_path is 108 bytes (RELGATE1 F3).
SCRATCH_ROOT="$WT/.cache/rg3b"
declare -A BIN=([v022]="$WT/.cache/rg3/v022-target/release/acos" \
                [v030]="$WT/.cache/rg3-target/release/acos")
REPS="${REPS:-3}"
mkdir -p "$OUT" "$SCRATCH_ROOT"

for arm in v022 v030; do
    printf 'arm %s binary %s sha256 %s\n' "$arm" "${BIN[$arm]}" \
        "$(sha256sum "${BIN[$arm]}" | cut -d' ' -f1)" >> "$OUT/arms.txt"
done
"${BIN[v022]}" --version >> "$OUT/arms.txt"
"${BIN[v030]}" --version >> "$OUT/arms.txt"

for rep in $(seq 1 "$REPS"); do
    for arm in v022 v030; do
        for grp in "latency,cold:8G:540" "memory:16G:600"; do
            phases="${grp%%:*}"; rest="${grp#*:}"
            mem="${rest%%:*}"; tmax="${rest##*:}"
            outdir="$OUT/run-$arm/rep-$rep"
            mkdir -p "$outdir"
            scratch="$SCRATCH_ROOT/r$rep-$arm-${phases%%,*}"
            rm -rf "$scratch"; mkdir -p "$scratch"
            echo "=== $(date -u +%FT%TZ) rep=$rep arm=$arm phases=$phases mem=$mem load1=$(cut -d' ' -f1 /proc/loadavg)"
            "$WT/tools/art-runtime/run-exclusive.sh" --task RELGATE3 --exclusive -- \
                "$WT/tools/guarded.sh" --mem "$mem" --time "$tmax" -- \
                python3 "$BENCH" --bundle "$phases" --acos "${BIN[$arm]}" \
                --out-dir "$outdir" --scratch-root "$scratch" \
                --reps 20 --fast-reps 40 --p11-reps 3 --loops 0 --animations on
            rc=$?
            echo "=== $(date -u +%FT%TZ) rep=$rep arm=$arm phases=$phases rc=$rc load1=$(cut -d' ' -f1 /proc/loadavg)"
            rm -rf "$scratch"
        done
    done
done

# Merge each arm's three reps into one document with raw samples.
for arm in v022 v030; do
    python3 "$BENCH" --merge --run-dir "$OUT/run-$arm" --acos "${BIN[$arm]}" \
        --out "$OUT/$arm.json" --markdown "$OUT/$arm.md" --mode full
    echo "merge $arm rc=$?"
done

echo "=== DRIVER DONE $(date -u +%FT%TZ)"
