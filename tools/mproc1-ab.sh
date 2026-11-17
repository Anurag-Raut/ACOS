#!/usr/bin/env bash
#
# MPROC1 single-app A/B driver (PREPARED, NOT RUN here).
#
# The common brief forbids an implementer's own exclusive A/B: timed A/Bs are
# batched and run by the bench owner. This script is the arm runner the bench
# owner (or the orchestrator) executes; it only *runs* when invoked.
#
# Question it answers: does the multi-package / multi-process work cost a
# single-app session anything? Two arms:
#
#   A  `mproc0` (4d793e73) — multi-package PMS, no launch plumbing, no capture
#      target, no ClipData codec, one guest process (the pre-MPRO1 behaviour).
#   B  `mproc1` (this branch) — everything in A plus `extra_apks`, the intent
#      capture target and the ClipData codec.
#
# Both arms run the SAME single-app phases with NO extra APKs, so the delta is
# the cost of the seam, not of a feature being used. Expected: no regression
# beyond the bench's own noise rule (`max(category floor, 2x worst rep-to-rep
# deviation)`, capped at 40 %).
#
# What it measures (single-app only):
#   memory  idle memory, 1 app            (phase `memory`, --mem 16G)
#   cold    cold start                    (phase `cold`, M3)
#   latency warm launch / tree / reset / tap, strict + fast
#           (phase `latency`, M4/M6/M7/M8 -- where the seam could cost)
#
# Usage (bench owner):
#
#   # 1. build both arms into their own target dirs (no shared lock needed)
#   cd <worktree A>; git checkout mproc0
#   CARGO_TARGET_DIR=<A>/.cache/ab-target CARGO_PROFILE_DEV_DEBUG=0 \
#     cargo build --release -p acos-cli -p acos-art
#   cd <worktree B>; git checkout mproc1
#   CARGO_TARGET_DIR=<B>/.cache/ab-target CARGO_PROFILE_DEV_DEBUG=0 \
#     cargo build --release -p acos-cli -p acos-art
#
#   # 2. run the arms (each acquires the shared lock per phase internally)
#   tools/mproc1-ab.sh --arm a --root <worktree A> --target <A>/.cache/ab-target
#   tools/mproc1-ab.sh --arm b --root <worktree B> --target <B>/.cache/ab-target
#   tools/mproc1-ab.sh --compare <run-a-dir> <run-b-dir>
#
# The script never invents numbers: it shells out to tools/bench/acos-bench.sh
# with `--no-build --phases mem1,cold,launch,tree` and then compares the two
# stored runs with tools/bench/ab-compare.py. A missing run directory or a
# failed phase is a hard error.
set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

usage() { sed -n '2,60p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'; }

mode=""
arm=""
worktree=""
target=""
runs_a=""
runs_b=""
reps=3
while [ $# -gt 0 ]; do
    case "$1" in
        --arm) mode=arm; arm="$2"; shift 2 ;;
        --root) worktree="$2"; shift 2 ;;
        --target) target="$2"; shift 2 ;;
        --reps) reps="$2"; shift 2 ;;
        --compare) mode=compare; runs_a="$2"; runs_b="$3"; shift 3 ;;
        -h|--help) usage; exit 0 ;;
        *) echo "mproc1-ab: unknown option $1" >&2; usage >&2; exit 2 ;;
    esac
done

if [ "$mode" = arm ]; then
    case "$arm" in
        a|b) ;;
        *) echo "mproc1-ab: --arm must be a or b" >&2; exit 2 ;;
    esac
    [ -n "$worktree" ] && [ -d "$worktree" ] || { echo "mproc1-ab: --root must be a worktree" >&2; exit 2; }
    [ -n "$target" ] && [ -d "$target" ] || { echo "mproc1-ab: --target must be a built target dir" >&2; exit 2; }
    out="$ROOT/results/bench/mproc1-ab-$arm"
    mkdir -p "$out"
    # `acos-bench.sh` reads the arm's binaries from its own checkout + target;
    # run it from that worktree so `resolve_binary` finds `<worktree>/.cache/...`
    # through CARGO_TARGET_DIR, and pin ACOS_ART_BIN explicitly so the engine
    # never falls back to another arm's binary.
    exec env \
        CARGO_TARGET_DIR="$target" \
        ACOS_ART_BIN="$target/release/acos-art" \
        ACOS_BENCH_TASK="MPROC1-AB-$arm" \
        ACOS_BENCH_SCRATCH="$ROOT/.cache/mproc1-ab-$arm" \
        "$worktree/tools/bench/acos-bench.sh" \
        --no-build \
        --reps "$reps" \
        --phases memory,cold,latency \
        --out "$out/baseline.json" \
        --markdown "$out/baseline.md" \
        --run-dir "$out/run"
fi

if [ "$mode" = compare ]; then
    [ -d "$runs_a" ] || { echo "mproc1-ab: $runs_a is not a run dir" >&2; exit 2; }
    [ -d "$runs_b" ] || { echo "mproc1-ab: $runs_b is not a run dir" >&2; exit 2; }
    exec python3 "$ROOT/tools/bench/ab-compare.py" \
        --a "$runs_a"/*.json \
        --b "$runs_b"/*.json
fi

usage >&2
exit 2
