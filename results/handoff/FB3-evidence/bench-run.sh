#!/usr/bin/env bash
# FB3 M8 A/B: interleaved arms, 3 reps each, one lock acquisition per invocation.
set -uo pipefail
WT=/home/Qui/.t3/worktrees/acos/t3-d41b3b08-5ea0b20149a5a9a9
cd "$WT"
export ACOS_BENCH_TASK=FB3
B="$WT/.cache/fb3/bench"
mkdir -p "$B"
run_arm() {
    local arm="$1" rep="$2"
    local out="$B/${arm}${rep}.json" md="$B/${arm}${rep}.md" rd="$B/run-${arm}${rep}" sr="$WT/.cache/h6"
    echo "=== $(date -Is) arm=$arm rep=$rep" 
    tools/bench/acos-bench.sh --reps 1 --groups "a:latency:8G:300" \
        --target-dir "$WT/.cache/fb3/arm${arm}" --no-build \
        --out "$out" --markdown "$md" --run-dir "$rd" --scratch-root "$sr" \
        > "$B/${arm}${rep}.log" 2>&1
    echo "=== $(date -Is) arm=$arm rep=$rep exit=$?"
}
run_arm A 1
run_arm B 1
run_arm A 2
run_arm B 2
run_arm A 3
run_arm B 3
echo "=== $(date -Is) AB done"
