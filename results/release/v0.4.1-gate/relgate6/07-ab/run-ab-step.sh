#!/usr/bin/env bash
# Run ONE group of Step 7 A/B: run-ab-step.sh <rep> <arm> <latency,cold|memory>
# Designed so each invocation finishes in ~3-5 min (inside the 600s command timeout).
set -uo pipefail
REP="${1:?usage: $0 <rep> <arm> <latency,cold|memory>}"
ARM="${2:?}"
PHASES="${3:?}"

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
SCRATCH_ROOT="$WT/.cache/rg6b"
declare -A BIN=(
  [v040]="$BASE/.cache/target/release/acos"
  [v041]="$WT/.cache/target/release/acos"
)
declare -A AROOT=(
  [v040]="$BASE"
  [v041]="$WT"
)
mkdir -p "$OUT" "$SCRATCH_ROOT"

if [ ! -s "$OUT/arms.txt" ]; then
    for a in v040 v041; do
        printf 'arm %s root %s binary %s sha256 %s\n' "$a" "${AROOT[$a]}" "${BIN[$a]}" \
            "$(sha256sum "${BIN[$a]}" | cut -d' ' -f1)" >> "$OUT/arms.txt"
    done
    "${BIN[v040]}" --version >> "$OUT/arms.txt"
    "${BIN[v041]}" --version >> "$OUT/arms.txt"
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
"$WT/tools/art-runtime/run-exclusive.sh" --task RELGATE6-ab --exclusive -- \
    "$WT/tools/guarded.sh" --mem "$mem" --time "$tmax" -- \
    python3 "$BENCH" --bundle "$PHASES" --acos "${BIN[$ARM]}" \
    --out-dir "$outdir" --scratch-root "$scratch" \
    --reps 20 --fast-reps 40 --p11-reps 3 --loops 0 --animations on 2>&1 | tee -a "$OUT/driver.log"
rc=${PIPESTATUS[0]}
echo "=== $(date -u +%FT%TZ) rep=$REP arm=$ARM phases=$PHASES rc=$rc load1=$(cut -d' ' -f1 /proc/loadavg)" | tee -a "$OUT/driver.log"
rm -rf "$scratch"
exit "$rc"
