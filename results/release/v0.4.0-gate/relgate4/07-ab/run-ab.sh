#!/usr/bin/env bash
# RELGATE4 step 7: interleaved A/B (v0.3.0 tag 02421e17b vs v0.4.0 RC afad66858)
# on the exact same lattice and options as RELGATE3 (3 reps, --exclusive),
# plus the RC `install` phase (m9_apk_ready_ms) for Step 6's emu_gate.py.
set -uo pipefail
WT="/home/Qui/.t3/worktrees/acos/t3-f4bcc4d4-5c49b0ef4f83b639"
cd "$WT" || exit 2
export ACOS_FONT_DIR=/home/Qui/Projects/acos/.cache/boot/android-34/fonts
unset ACOS_BOOT_DIR XDG_RUNTIME_DIR ACOS_HOME TMPDIR ACOS_ART_BIN
export ACOS_BENCH_RUN_ID="relgate4-ab"
export ACOS_BENCH_PIDFILE="$WT/.cache/rg4-ab.pids"
: > "$ACOS_BENCH_PIDFILE"
. "$WT/tools/bench/reap-daemons.sh"
trap 'bench_reap_daemons' EXIT INT TERM

BENCH="$WT/tools/bench/acos_bench.py"
OUT="$WT/results/release/v0.4.0-gate/relgate4/07-ab"
EMU_OUT="$WT/results/release/v0.4.0-gate/relgate4/06-emubench"
# Short scratch root so <scratch>/serve.sock fits SUN_LEN (108 bytes).
SCRATCH_ROOT="$WT/.cache/rg4b"
declare -A BIN=(
  [v030]="$WT/.cache/rg4-v030/target/release/acos"
  [v040]="$WT/.cache/rg4-target/release/acos"
)
declare -A AROOT=(
  [v030]="$WT/.cache/rg4-v030/src"
  [v040]="$WT"
)
REPS="${REPS:-3}"
mkdir -p "$OUT" "$EMU_OUT" "$SCRATCH_ROOT"
: > "$OUT/arms.txt"

for arm in v030 v040; do
    printf 'arm %s root %s binary %s sha256 %s\n' "$arm" "${AROOT[$arm]}" "${BIN[$arm]}" \
        "$(sha256sum "${BIN[$arm]}" | cut -d' ' -f1)" >> "$OUT/arms.txt"
done
"${BIN[v030]}" --version >> "$OUT/arms.txt"
"${BIN[v040]}" --version >> "$OUT/arms.txt"

for rep in $(seq 1 "$REPS"); do
    for arm in v030 v040; do
        for grp in "latency,cold:8G:540" "memory:16G:600"; do
            phases="${grp%%:*}"; rest="${grp#*:}"
            mem="${rest%%:*}"; tmax="${rest##*:}"
            outdir="$OUT/run-$arm/rep-$rep"
            mkdir -p "$outdir"
            scratch="$SCRATCH_ROOT/r$rep-$arm-${phases%%,*}"
            rm -rf "$scratch"; mkdir -p "$scratch"
            echo "=== $(date -u +%FT%TZ) rep=$rep arm=$arm phases=$phases mem=$mem load1=$(cut -d' ' -f1 /proc/loadavg) memavail=$(awk '/MemAvailable/{print int($2/1024)}' /proc/meminfo)MiB"
            ACOS_ROOT="${AROOT[$arm]}" \
            "$WT/tools/art-runtime/run-exclusive.sh" --task RELGATE4-ab --exclusive -- \
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
for arm in v030 v040; do
    python3 "$BENCH" --merge --run-dir "$OUT/run-$arm" --acos "${BIN[$arm]}" \
        --out "$OUT/$arm.json" --markdown "$OUT/$arm.md" --mode full
    echo "merge $arm rc=$?"
done

# Run lattice-aware A/B comparison
python3 "$WT/tools/bench/ab-compare.py" \
    --a "$OUT/v030.json" --b "$OUT/v040.json" \
    --label-a "v0.3.0 (02421e17b)" --label-b "v0.4.0 RC (afad66858)" \
    --json "$OUT/ab-compare.json" \
    > "$OUT/ab-compare.txt" 2>&1
echo "ab-compare rc=$?"

# Run RC install phase (m9_apk_ready_ms) for Step 6 emu_gate.py
scratch="$SCRATCH_ROOT/install-v040"
rm -rf "$scratch"; mkdir -p "$scratch"
echo "=== $(date -u +%FT%TZ) RC install phase (m9_apk_ready_ms) load1=$(cut -d' ' -f1 /proc/loadavg)"
ACOS_ROOT="${AROOT[v040]}" \
"$WT/tools/art-runtime/run-exclusive.sh" --task RELGATE4-inst --exclusive -- \
    "$WT/tools/guarded.sh" --mem 8G --time 300 -- \
    python3 "$BENCH" --phase install --acos "${BIN[v040]}" \
    --out "$EMU_OUT/acos-install-phase.json" --scratch "$scratch" \
    --reps 5 --animations on
echo "=== $(date -u +%FT%TZ) RC install phase rc=$?"
rm -rf "$scratch"

echo "=== DRIVER DONE $(date -u +%FT%TZ)"
df -h / | tail -1
