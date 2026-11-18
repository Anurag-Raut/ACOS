#!/usr/bin/env bash
# BACKSETTLE indicative A/B (base 8b3afbdd vs backsettle), SHARED slot: 5 reps.
#
# Not an exclusive gate run (the bench owner's): one shared slot per
# acquisition, other agents' ART runs may share the host. Per rep the arms are
# interleaved (A latency, B latency); ADR-0028 sample counts (20 strict /
# 40 fast). The metric this change could move is M8 tap->settled counter (fast):
# the window-forest gate is built only for a strict input action, so `fast`
# takes no lock and no extra work - the run is here to show that, not to claim
# a release number.
#
# Usage: REPS=5 run-ab.sh
set -uo pipefail
WT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
cd "$WT" || exit 2
export ACOS_FONT_DIR=/home/Qui/Projects/acos/.cache/boot/android-34/fonts
export ACOS_RUNTIME_DIR="$WT/.cache/rt-backsettle"
export ACOS_SESSION_SOCKET="$WT/.cache/rt-backsettle/session.sock"
export ACOS_VIEW_TEST_DIR="$WT/.cache/lv"
unset ACOS_BOOT_DIR XDG_RUNTIME_DIR ACOS_HOME

BENCH="$WT/tools/bench/acos_bench.py"
OUT="${AB_OUT:-$WT/results/handoff/BACKSETTLE/bench}"
SCRATCH_ROOT="$WT/.cache/bench-bs"
declare -A BIN=([base]="$WT/.cache/basewt/.cache/base-target/release/acos" \
                [fix]="$WT/.cache/backsettle-target/release/acos")
REPS="${REPS:-5}"
mkdir -p "$OUT" "$SCRATCH_ROOT"

for arm in base fix; do
    printf 'arm %s binary %s sha256 %s\n' "$arm" "${BIN[$arm]}" \
        "$(sha256sum "${BIN[$arm]}" | cut -d' ' -f1)" >> "$OUT/arms.txt"
    "${BIN[$arm]}" --version >> "$OUT/arms.txt"
done

for rep in $(seq 1 "$REPS"); do
    for arm in base fix; do
        outdir="$OUT/run-$arm/rep-$rep"
        mkdir -p "$outdir"
        scratch="$SCRATCH_ROOT/r$rep-$arm"
        rm -rf "$scratch"; mkdir -p "$scratch"
        echo "=== $(date -u +%FT%TZ) rep=$rep arm=$arm load1=$(cut -d' ' -f1 /proc/loadavg)"
        "$WT/tools/art-runtime/run-exclusive.sh" --task BACKSETTLE -- \
            "$WT/tools/guarded.sh" --mem 8G --time 540 -- \
            python3 "$BENCH" --bundle latency --acos "${BIN[$arm]}" \
            --out-dir "$outdir" --scratch-root "$scratch" \
            --reps 20 --fast-reps 40 --p11-reps 3 --loops 0 --animations on
        echo "=== $(date -u +%FT%TZ) rep=$rep arm=$arm rc=$? load1=$(cut -d' ' -f1 /proc/loadavg)"
        rm -rf "$scratch"
    done
done

for arm in base fix; do
    python3 "$BENCH" --merge --run-dir "$OUT/run-$arm" --acos "${BIN[$arm]}" \
        --out "$OUT/$arm.json" --markdown "$OUT/$arm.md" --mode full
    echo "merge $arm rc=$?"
done

python3 "$WT/tools/bench/ab-compare.py" --a "$OUT/base.json" --b "$OUT/fix.json" \
    --label-a "base 8b3afbdd" --label-b "backsettle" \
    --json "$OUT/ab-compare.json" 2>&1 | tee "$OUT/ab-compare.txt" | tail -40

echo "=== BENCH DONE $(date -u +%FT%TZ)"
