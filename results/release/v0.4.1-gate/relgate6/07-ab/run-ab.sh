#!/usr/bin/env bash
# RELGATE6 step 7: interleaved A/B (v0.4.0 8e020cee3 vs v0.4.1 fix041 a84f915bb)
# on the exact same lattice and options as RELGATE5 (3 reps, --exclusive).
set -uo pipefail
WT="/home/Qui/.t3/worktrees/acos/t3-30ef7cb0-4a8f6f576a950a8e"
BASE="/home/Qui/.t3/worktrees/acos/timing-base-8e020"
cd "$WT" || exit 2
export ACOS_FONT_DIR=/home/Qui/Projects/acos/.cache/boot/android-34/fonts
unset ACOS_BOOT_DIR XDG_RUNTIME_DIR ACOS_HOME TMPDIR ACOS_ART_BIN
export ACOS_BENCH_RUN_ID="relgate6-ab"
export ACOS_BENCH_PIDFILE="$WT/.cache/rg6-ab.pids"
: > "$ACOS_BENCH_PIDFILE"
. "$WT/tools/bench/reap-daemons.sh"
trap 'bench_reap_daemons' EXIT INT TERM

BENCH="$WT/tools/bench/acos_bench.py"
OUT="$WT/results/release/v0.4.1-gate/relgate6/07-ab"
# Short scratch root so <scratch>/serve.sock fits SUN_LEN (108 bytes).
SCRATCH_ROOT="$WT/.cache/rg6b"
declare -A BIN=(
  [v040]="$BASE/.cache/target/release/acos"
  [v041]="$WT/.cache/target/release/acos"
)
declare -A AROOT=(
  [v040]="$BASE"
  [v041]="$WT"
)
REPS="${REPS:-3}"
mkdir -p "$OUT" "$SCRATCH_ROOT"
: > "$OUT/arms.txt"
: > "$OUT/driver.log"

for arm in v040 v041; do
    printf 'arm %s root %s binary %s sha256 %s\n' "$arm" "${AROOT[$arm]}" "${BIN[$arm]}" \
        "$(sha256sum "${BIN[$arm]}" | cut -d' ' -f1)" >> "$OUT/arms.txt"
done
"${BIN[v040]}" --version >> "$OUT/arms.txt"
"${BIN[v041]}" --version >> "$OUT/arms.txt"

for rep in $(seq 1 "$REPS"); do
    for arm in v040 v041; do
        for grp in "latency,cold:8G:540" "memory:16G:600"; do
            phases="${grp%%:*}"; rest="${grp#*:}"
            mem="${rest%%:*}"; tmax="${rest##*:}"
            outdir="$OUT/run-$arm/rep-$rep"
            mkdir -p "$outdir"
            scratch="$SCRATCH_ROOT/r$rep-$arm-${phases%%,*}"
            rm -rf "$scratch"; mkdir -p "$scratch"
            echo "=== $(date -u +%FT%TZ) rep=$rep arm=$arm phases=$phases mem=$mem load1=$(cut -d' ' -f1 /proc/loadavg) memavail=$(awk '/MemAvailable/{print int($2/1024)}' /proc/meminfo)MiB" | tee -a "$OUT/driver.log"
            ACOS_ROOT="${AROOT[$arm]}" \
            "$WT/tools/art-runtime/run-exclusive.sh" --task RELGATE6-ab --exclusive -- \
                "$WT/tools/guarded.sh" --mem "$mem" --time "$tmax" -- \
                python3 "$BENCH" --bundle "$phases" --acos "${BIN[$arm]}" \
                --out-dir "$outdir" --scratch-root "$scratch" \
                --reps 20 --fast-reps 40 --p11-reps 3 --loops 0 --animations on 2>&1 | tee -a "$OUT/driver.log"
            rc=${PIPESTATUS[0]}
            echo "=== $(date -u +%FT%TZ) rep=$rep arm=$arm phases=$phases rc=$rc load1=$(cut -d' ' -f1 /proc/loadavg)" | tee -a "$OUT/driver.log"
            rm -rf "$scratch"
        done
    done
done

# Merge each arm's three reps into one document with raw samples.
for arm in v040 v041; do
    python3 "$BENCH" --merge --run-dir "$OUT/run-$arm" --acos "${BIN[$arm]}" \
        --out "$OUT/$arm.json" --markdown "$OUT/$arm.md" --mode full 2>&1 | tee -a "$OUT/driver.log"
    echo "merge $arm rc=${PIPESTATUS[0]}" | tee -a "$OUT/driver.log"
done

# Run lattice-aware A/B comparison
python3 "$WT/tools/bench/ab-compare.py" \
    --a "$OUT/v040.json" --b "$OUT/v041.json" \
    --label-a "v0.4.0 (8e020cee3)" --label-b "v0.4.1 fix041 (a84f915bb)" \
    --json "$OUT/ab-compare.json" \
    > "$OUT/ab-compare.txt" 2>&1
echo "ab-compare rc=$?" | tee -a "$OUT/driver.log"

rm -rf "$SCRATCH_ROOT"
echo "=== DRIVER DONE $(date -u +%FT%TZ)" | tee -a "$OUT/driver.log"
df -h / | tail -1 | tee -a "$OUT/driver.log"
free -m | head -2 | tee -a "$OUT/driver.log"
