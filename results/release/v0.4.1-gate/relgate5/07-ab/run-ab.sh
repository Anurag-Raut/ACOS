#!/usr/bin/env bash
# RELGATE5 step 7: interleaved A/B (v0.4.0 tag 8e020cee3 vs v0.4.1 RC 36859df75)
# on the exact same lattice and options as RELGATE4 (3 reps, --exclusive),
# plus the RC `install` phase (m9_apk_ready_ms) for Step 6's emu_gate.py.
set -uo pipefail
WT="/home/Qui/.t3/worktrees/acos/t3-8432ccd8-1d58d051095d020b"
cd "$WT" || exit 2
export ACOS_FONT_DIR=/home/Qui/Projects/acos/.cache/boot/android-34/fonts
unset ACOS_BOOT_DIR XDG_RUNTIME_DIR ACOS_HOME TMPDIR ACOS_ART_BIN
export ACOS_BENCH_RUN_ID="relgate5-ab"
export ACOS_BENCH_PIDFILE="$WT/.cache/rg5-ab.pids"
: > "$ACOS_BENCH_PIDFILE"
. "$WT/tools/bench/reap-daemons.sh"
trap 'bench_reap_daemons' EXIT INT TERM

BENCH="$WT/tools/bench/acos_bench.py"
OUT="$WT/results/release/v0.4.1-gate/relgate5/07-ab"
EMU_OUT="$WT/results/release/v0.4.1-gate/relgate5/06-emubench"
# Short scratch root so <scratch>/serve.sock fits SUN_LEN (108 bytes).
SCRATCH_ROOT="$WT/.cache/rg5b"
declare -A BIN=(
  [v040]="$WT/.cache/rg5-v040/target/release/acos"
  [v041]="$WT/.cache/rg5-target/release/acos"
)
declare -A AROOT=(
  [v040]="$WT/.cache/rg5-v040/src"
  [v041]="$WT"
)
REPS="${REPS:-3}"
mkdir -p "$OUT" "$EMU_OUT" "$SCRATCH_ROOT"
: > "$OUT/arms.txt"

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
            echo "=== $(date -u +%FT%TZ) rep=$rep arm=$arm phases=$phases mem=$mem load1=$(cut -d' ' -f1 /proc/loadavg) memavail=$(awk '/MemAvailable/{print int($2/1024)}' /proc/meminfo)MiB"
            ACOS_ROOT="${AROOT[$arm]}" \
            "$WT/tools/art-runtime/run-exclusive.sh" --task RELGATE5-ab --exclusive -- \
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
for arm in v040 v041; do
    python3 "$BENCH" --merge --run-dir "$OUT/run-$arm" --acos "${BIN[$arm]}" \
        --out "$OUT/$arm.json" --markdown "$OUT/$arm.md" --mode full
    echo "merge $arm rc=$?"
done

# Run lattice-aware A/B comparison
python3 "$WT/tools/bench/ab-compare.py" \
    --a "$OUT/v040.json" --b "$OUT/v041.json" \
    --label-a "v0.4.0 (8e020cee3)" --label-b "v0.4.1 RC (36859df75)" \
    --json "$OUT/ab-compare.json" \
    > "$OUT/ab-compare.txt" 2>&1
echo "ab-compare rc=$?"

# Run RC install phase (m9_apk_ready_ms) for Step 6 emu_gate.py
scratch="$SCRATCH_ROOT/install-v041"
rm -rf "$scratch"; mkdir -p "$scratch"
echo "=== $(date -u +%FT%TZ) RC install phase (m9_apk_ready_ms) load1=$(cut -d' ' -f1 /proc/loadavg)"
ACOS_ROOT="${AROOT[v041]}" \
"$WT/tools/art-runtime/run-exclusive.sh" --task RELGATE5-inst --exclusive -- \
    "$WT/tools/guarded.sh" --mem 8G --time 300 -- \
    python3 "$BENCH" --phase install --acos "${BIN[v041]}" \
    --out "$EMU_OUT/acos-install-phase.json" --scratch "$scratch" \
    --reps 5 --animations on
echo "=== $(date -u +%FT%TZ) RC install phase rc=$?"
rm -rf "$scratch" "$SCRATCH_ROOT"

echo "=== DRIVER DONE $(date -u +%FT%TZ)"
df -h / | tail -1
free -m | head -2
