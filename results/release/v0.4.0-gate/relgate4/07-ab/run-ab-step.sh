#!/usr/bin/env bash
# Run ONE group of Step 7 A/B: run-ab-step.sh <rep> <arm> <latency,cold|memory|install>
# Designed so each invocation finishes in ~3-4 min (well inside the 600s command timeout).
set -uo pipefail
REP="${1:?usage: $0 <rep> <arm> <latency,cold|memory|install>}"
ARM="${2:?}"
PHASES="${3:?}"

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
SCRATCH_ROOT="$WT/.cache/rg4b"
declare -A BIN=(
  [v030]="$WT/.cache/rg4-v030/target/release/acos"
  [v040]="$WT/.cache/rg4-target/release/acos"
)
declare -A AROOT=(
  [v030]="$WT/.cache/rg4-v030/src"
  [v040]="$WT"
)
mkdir -p "$OUT" "$EMU_OUT" "$SCRATCH_ROOT"

if [ "$PHASES" = "install" ]; then
    scratch="$SCRATCH_ROOT/install-$ARM"
    rm -rf "$scratch"; mkdir -p "$scratch"
    echo "=== $(date -u +%FT%TZ) arm=$ARM phase=install load1=$(cut -d' ' -f1 /proc/loadavg)" | tee -a "$OUT/driver.log"
    ACOS_ROOT="${AROOT[$ARM]}" \
    "$WT/tools/art-runtime/run-exclusive.sh" --task RELGATE4-inst --exclusive -- \
        "$WT/tools/guarded.sh" --mem 8G --time 300 -- \
        python3 "$BENCH" --phase install --acos "${BIN[$ARM]}" \
        --out "$EMU_OUT/acos-install-phase.json" --scratch "$scratch" \
        --reps 5 --animations on 2>&1 | tee -a "$OUT/driver.log"
    rc=${PIPESTATUS[0]}
    echo "=== $(date -u +%FT%TZ) arm=$ARM phase=install rc=$rc load1=$(cut -d' ' -f1 /proc/loadavg)" | tee -a "$OUT/driver.log"
    rm -rf "$scratch"
    exit "$rc"
fi

case "$PHASES" in
    latency,cold) mem=8G; tmax=540 ;;
    memory)       mem=16G; tmax=540 ;;
    *) echo "unknown phases: $PHASES" >&2; exit 2 ;;
esac

outdir="$OUT/run-$ARM/rep-$REP"
mkdir -p "$outdir"
scratch="$SCRATCH_ROOT/r$REP-$ARM-${PHASES%%,*}"
rm -rf "$scratch"; mkdir -p "$scratch"
echo "=== $(date -u +%FT%TZ) rep=$REP arm=$ARM phases=$PHASES mem=$mem load1=$(cut -d' ' -f1 /proc/loadavg) memavail=$(awk '/MemAvailable/{print int($2/1024)}' /proc/meminfo)MiB" | tee -a "$OUT/driver.log"
ACOS_ROOT="${AROOT[$ARM]}" \
"$WT/tools/art-runtime/run-exclusive.sh" --task RELGATE4-ab --exclusive -- \
    "$WT/tools/guarded.sh" --mem "$mem" --time "$tmax" -- \
    python3 "$BENCH" --bundle "$PHASES" --acos "${BIN[$ARM]}" \
    --out-dir "$outdir" --scratch-root "$scratch" \
    --reps 20 --fast-reps 40 --p11-reps 3 --loops 0 --animations on 2>&1 | tee -a "$OUT/driver.log"
rc=${PIPESTATUS[0]}
echo "=== $(date -u +%FT%TZ) rep=$REP arm=$ARM phases=$PHASES rc=$rc load1=$(cut -d' ' -f1 /proc/loadavg)" | tee -a "$OUT/driver.log"
rm -rf "$scratch"
exit "$rc"
