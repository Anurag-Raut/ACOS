#!/usr/bin/env bash
#
# layer0-single-window-ab.sh -- LAYER0/LAYER1 single-window fast-path A/B.
#
# PREPARED, NOT RUN BY THE IMPLEMENTER. The bench owner runs this once per
# release candidate under the exclusive lock: a timed A/B blocks every other
# agent's ART run, so the driver is checked in and scheduled centrally (see
# results/handoff/LAYER1/RUNBOOK.md).
#
# What it measures:
#   * `acos_bench.sh --phases latency,cold` on both arms, with the same bundles
#     and reps as BENCHFIX-AB (3 reps, 20 latency reps, 40 fast samples per rep
#     by default), so the numbers are directly comparable with the stored
#     BENCHFIX baseline;
#   * `ab-compare.py` over the two arms' merged JSONs (old gate + lattice view);
#   * the C1 composite micro-check: `cargo test -p acos-services --test
#     w35_composite` on each arm. It asserts the one-layer composite is the
#     byte-identical pass-through (`a_single_full_window_layer_passes_through_with_its_own_stride`),
#     i.e. the stack did not put a two-layer path under a single-window app.
#
# Usage:
#   tools/bench/layer0-single-window-ab.sh \
#       --a-bin /path/to/main/target/release/acos \
#       --b-bin /path/to/layer1/.cache/layer1-target/release/acos \
#       [--a-test-dir /path/to/main] [--b-test-dir /path/to/layer1] \
#       [--reps 3] [--latency-reps 20] [--fast-reps 40] \
#       [--out-dir results/bench/layer0-ab-<UTC>] [--task LAYER1]
#
# --a-test-dir/--b-test-dir are optional: when given, the micro-check runs in
# that checkout with CARGO_TARGET_DIR under it (the bench owner's build). Both
# binaries must be `--release` builds of the same commit the test dirs belong
# to; the driver records their sha256 so a mixed pair is visible in the report.
set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT"

UTC="$(date -u +%Y%m%dT%H%M%SZ)"
OUT_DIR="$ROOT/results/bench/layer0-ab-$UTC"
TASK="${ACOS_BENCH_TASK:-LAYER1}"
REPS=3
LATENCY_REPS=20
FAST_REPS=40
A_BIN=""
B_BIN=""
A_TEST_DIR=""
B_TEST_DIR=""

usage() { sed -n '2,40p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'; }

while [ $# -gt 0 ]; do
    case "$1" in
        --a-bin) A_BIN="$2"; shift 2 ;;
        --b-bin) B_BIN="$2"; shift 2 ;;
        --a-test-dir) A_TEST_DIR="$2"; shift 2 ;;
        --b-test-dir) B_TEST_DIR="$2"; shift 2 ;;
        --reps) REPS="$2"; shift 2 ;;
        --latency-reps) LATENCY_REPS="$2"; shift 2 ;;
        --fast-reps) FAST_REPS="$2"; shift 2 ;;
        --out-dir) OUT_DIR="$2"; shift 2 ;;
        --task) TASK="$2"; shift 2 ;;
        -h|--help) usage; exit 0 ;;
        *) echo "layer0-single-window-ab: unknown option $1" >&2; usage >&2; exit 2 ;;
    esac
done

die() { echo "layer0-single-window-ab: error: $*" >&2; exit 2; }
[ -n "$A_BIN" ] || die "--a-bin (arm A, e.g. main) is required"
[ -n "$B_BIN" ] || die "--b-bin (arm B, e.g. layer1) is required"
for b in "$A_BIN" "$B_BIN"; do
    [ -x "$b" ] || die "not an executable acos binary: $b"
    case "$b" in */release/*) ;; *) die "$b is not a release build (the A/B must not compare debug binaries)";; esac
done
[ "$A_BIN" != "$B_BIN" ] || die "arms A and B are the same binary; nothing to compare"

A_SHA="$(sha256sum "$A_BIN" | awk '{print $1}')"
B_SHA="$(sha256sum "$B_BIN" | awk '{print $1}')"
[ "$A_SHA" != "$B_SHA" ] || die "arms A and B have identical bytes; build arm B from the layer1 branch"

mkdir -p "$OUT_DIR"
echo "layer0-single-window-ab: A=$A_BIN ($A_SHA)"
echo "layer0-single-window-ab: B=$B_BIN ($B_SHA)"
echo "layer0-single-window-ab: out=$OUT_DIR reps=$REPS latency_reps=$LATENCY_REPS fast_reps=$FAST_REPS task=$TASK"

run_arm() {
    local label="$1" bin="$2"
    echo "layer0-single-window-ab: arm $label: latency,cold"
    "$ROOT/tools/bench/acos-bench.sh" \
        --phases latency,cold \
        --reps "$REPS" \
        --latency-reps "$LATENCY_REPS" \
        --fast-reps "$FAST_REPS" \
        --task "$TASK" \
        --target-dir "$(dirname "$(dirname "$bin")")" \
        --no-build \
        --out "$OUT_DIR/$label.json" \
        --markdown "$OUT_DIR/$label.md" \
        --run-dir "$OUT_DIR/run-$label" \
        || return $?
}

# The bench's own driver installs ACOS_FONT_DIR and never ACOS_BOOT_DIR; keep
# the environment exactly as `acos-bench.sh` expects.
export ACOS_ART_BIN="${ACOS_ART_BIN:-$(dirname "$B_BIN")/acos-art}"

RC=0
run_arm a "$A_BIN" || RC=$?
run_arm b "$B_BIN" || RC=$?

if [ -s "$OUT_DIR/a.json" ] && [ -s "$OUT_DIR/b.json" ]; then
    python3 "$ROOT/tools/bench/ab-compare.py" \
        --a "$OUT_DIR/a.json" \
        --b "$OUT_DIR/b.json" \
        --label-a "main ($A_SHA)" \
        --label-b "layer1 ($B_SHA)" \
        --json "$OUT_DIR/ab.json" > "$OUT_DIR/ab.txt" 2>&1
    cat "$OUT_DIR/ab.txt"
else
    echo "layer0-single-window-ab: one arm produced no merged JSON; see $OUT_DIR" >&2
    RC=1
fi

microcheck() {
    local label="$1" dir="$2"
    [ -n "$dir" ] || return 0
    [ -d "$dir" ] || { echo "layer0-single-window-ab: $label test dir missing: $dir" >&2; return 1; }
    echo "layer0-single-window-ab: C1 one-layer composite micro-check ($label)"
    ( cd "$dir" && CARGO_TARGET_DIR="$dir/.cache/layer1-ab-target" \
        CARGO_PROFILE_TEST_DEBUG=0 CARGO_INCREMENTAL=0 \
        "$ROOT/tools/guarded.sh" --mem 8G --time 600 -- \
        cargo test --release --offline -p acos-services --test w35_composite \
            a_single_full_window_layer_passes_through_with_its_own_stride \
            -- --exact ) | tee "$OUT_DIR/microcheck-$label.txt"
}
microcheck a "$A_TEST_DIR" || RC=1
microcheck b "$B_TEST_DIR" || RC=1

python3 - "$OUT_DIR/report.json" "$A_BIN" "$A_SHA" "$B_BIN" "$B_SHA" "$RC" <<'PY'
import json, sys, time
path, a_bin, a_sha, b_bin, b_sha, rc = sys.argv[1:7]
json.dump({
    "kind": "layer0-single-window-ab",
    "created_at": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
    "arm_a": {"bin": a_bin, "sha256": a_sha},
    "arm_b": {"bin": b_bin, "sha256": b_sha},
    "rc": int(rc),
    "note": "Prepared driver. The bench owner runs it exclusively; no timing "
            "number here is produced by the implementation agent.",
}, open(path, "w"), indent=1)
PY

echo "layer0-single-window-ab: done rc=$RC -> $OUT_DIR"
exit "$RC"
