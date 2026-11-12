#!/usr/bin/env bash
#
# tools/bench/acos-bench.sh -- H6: one reproducible command that measures every
# tracked ACOS metric on the current build and compares it with a stored baseline.
#
#   tools/bench/acos-bench.sh                       # full bench, 3 reps -> baseline
#   tools/bench/acos-bench.sh --quick               # CI subset, < 5 min under lock
#   tools/bench/acos-bench.sh --compare results/bench/BASELINE.json
#   tools/bench/acos-bench.sh --from-run results/bench/baseline-X.json \
#       --compare results/bench/BASELINE.json       # compare two stored runs
#   tools/bench/acos-bench.sh --accept --reps 3     # also write results/bench/BASELINE.json
#   tools/bench/acos-bench.sh --animations off --phases latency,loop1,loop5,loop10
#                                                   # H14 A/B arm: every launch
#                                                   # carries animations:off
#
# What it measures (method names in acos_bench.py's METRICS table):
#   M1 idle memory 1 app; M2 per extra session at N=1/5/10 (cgroup anon+shmem
#   AND Pss, both labelled); M3 cold start; M4 warm launch strict+fast; M5
#   screenshot; M6 reset strict+fast; M7 UI tree strict+fast; M8 tap->settled
#   strict+fast (counter + form); tap ack; GL1 acos_destroy teardown_ms; the
#   full P5 agent loop at N=1, N=5 (default) and N=10 (--phases loop10) (latency,
#   tasks/s, tasks/GiB-min); idle CPU% and wakeups/s at N=1/5/10 over a 60 s window plus active CPU during the
#   strict tap cohort; P11 settle-lab false passes (must be 0) and the
#   scenarios/p11-*.yaml sweep.
#
# Every ART phase runs as its own lock acquisition:
#   tools/art-runtime/run-exclusive.sh --task H6 -- \
#     tools/guarded.sh --mem 8G --time N -- python3 tools/bench/acos_bench.py ...
# (memory phase: --mem 16G), so the host-wide lock is never held for long and
# other agents can interleave. The lock task name is --task (default H6).
# Scratch defaults to /home/Qui/Projects/acos/.cache/h6/ (override with
# --scratch-root or ACOS_BENCH_SCRATCH; never /tmp). ACOS_FONT_DIR is exported;
# ACOS_BOOT_DIR is not set.
#
# --groups "<name>:<phases>:<mem>:<time>" runs a custom bundle in one lock
# acquisition (used by the H14 A/B arms). --animations on|off (or
# ACOS_BENCH_ANIMATIONS) is the process-wide launch arm for every phase and is
# recorded in the merged doc, so a compare across arms says "A/B", not
# "regression" (see acos_bench.py set_arm).
#
# Output:
#   results/bench/baseline-<UTC>.json / .md   merged baseline (pooled reps)
#   results/bench/run-<UTC>/rep-<k>/*.json     raw per-phase samples + meta.json
#   results/bench/BASELINE.json                only with --accept
#
# The compare step is p50-based (p95 is printed too but not gated) and exits
# non-zero only when a metric is worse by more than its per-metric threshold:
# max(category floor, 2 x worst rep-to-rep deviation), capped at 40 %, plus an
# absolute floor. Thresholds are derived from the measured repetitions and
# stored in the baseline JSON, so the rule is auditable.
set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT"

UTC="$(date -u +%Y%m%dT%H%M%SZ)"
STARTED_UTC="$(date -u +%FT%TZ)"
OUT="$ROOT/results/bench/baseline-$UTC.json"
MD="$ROOT/results/bench/baseline-$UTC.md"
RUN_DIR="$ROOT/results/bench/run-$UTC"
BASELINE="$ROOT/results/bench/BASELINE.json"
TARGET_DIR="${CARGO_TARGET_DIR:-$ROOT/target}"
SCRATCH_ROOT="${ACOS_BENCH_SCRATCH:-/home/Qui/Projects/acos/.cache/h6}"
TASK="${ACOS_BENCH_TASK:-H6}"
ANIMATIONS="${ACOS_BENCH_ANIMATIONS:-on}"
REPS=3
LATENCY_REPS=20
P11_REPS=3
LOOPS=0
QUICK=0
ACCEPT=0
BUILD=1
KEEP=0
COMPARE=""
FROM_RUN=""
PHASES=""
GROUP_SPEC=""
MEM_PHASE_MEM="16G"
OTHER_MEM="8G"

usage() { sed -n '2,50p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'; }

while [ $# -gt 0 ]; do
    case "$1" in
        --reps) REPS="$2"; shift 2 ;;
        --quick) QUICK=1; shift ;;
        --target-dir) TARGET_DIR="$2"; shift 2 ;;
        --out) OUT="$2"; shift 2 ;;
        --markdown) MD="$2"; shift 2 ;;
        --run-dir) RUN_DIR="$2"; shift 2 ;;
        --baseline) BASELINE="$2"; shift 2 ;;
        --phases) PHASES="$2"; shift 2 ;;
        --animations) ANIMATIONS="$2"; shift 2 ;;
        --groups) GROUP_SPEC="$2"; shift 2 ;;
        --task) TASK="$2"; shift 2 ;;
        --scratch-root) SCRATCH_ROOT="$2"; shift 2 ;;
        --latency-reps) LATENCY_REPS="$2"; shift 2 ;;
        --p11-reps) P11_REPS="$2"; shift 2 ;;
        --loops) LOOPS="$2"; shift 2 ;;
        --accept) ACCEPT=1; shift ;;
        --no-build) BUILD=0; shift ;;
        --keep-scratch) KEEP=1; shift ;;
        --compare) COMPARE="$2"; shift 2 ;;
        --from-run) FROM_RUN="$2"; shift 2 ;;
        -h|--help) usage; exit 0 ;;
        *) echo "acos-bench: unknown option $1" >&2; usage >&2; exit 2 ;;
    esac
done

# Phase groups: one group = one run-exclusive.sh acquisition (one queue wait).
# name:phases:mem:time. Every group stays under the 10-minute lock budget.
case "$ANIMATIONS" in
    on|off) ;;
    *) echo "acos-bench: --animations must be on|off (got $ANIMATIONS)" >&2; exit 2 ;;
esac
export ACOS_BENCH_ANIMATIONS="$ANIMATIONS"

if [ "$QUICK" -eq 1 ]; then
    REPS=1
    LATENCY_REPS=8
    P11_REPS=2
    LOOPS=0
    PHASE_GROUPS="a1:latency,loop1:8G:300
a2:p11,scenarios:8G:300"
else
    PHASE_GROUPS="a1:latency,cold:8G:420
a2:loop1,loop5,p11,scenarios:8G:540
mem:memory:16G:600"
fi
if [ -n "$GROUP_SPEC" ]; then
    # Custom group spec, e.g. "arm:latency,loop1,loop5:16G:700" (one lock
    # acquisition per line). Used for the H14 A/B arms.
    PHASE_GROUPS="$GROUP_SPEC"
elif [ -n "$PHASES" ]; then
    # Explicit --phases keeps one group per phase (used for focused runs).
    PHASE_GROUPS=""
    for p in $PHASES; do
        case "$p" in
            memory) p_mem="$MEM_PHASE_MEM"; p_time=600 ;;
            latency) p_mem="$OTHER_MEM"; p_time=600 ;;
            loop1) p_mem="$OTHER_MEM"; p_time=240 ;;
            loop5) p_mem="$OTHER_MEM"; p_time=300 ;;
            loop10) p_mem="$MEM_PHASE_MEM"; p_time=600 ;;
            cold) p_mem="$OTHER_MEM"; p_time=300 ;;
            p11) p_mem="$OTHER_MEM"; p_time=480 ;;
            scenarios) p_mem="$OTHER_MEM"; p_time=480 ;;
            *) p_mem="$OTHER_MEM"; p_time=300 ;;
        esac
        PHASE_GROUPS="$PHASE_GROUPS
g-$p:$p:$p_mem:$p_time"
    done
fi

BIN="$TARGET_DIR/release/acos"
PY="$ROOT/tools/bench/acos_bench.py"

# -- compare two stored runs, no measurement --------------------------------
if [ -n "$FROM_RUN" ] && [ -n "$COMPARE" ]; then
    exec python3 "$PY" --compare "$COMPARE" --from-run "$FROM_RUN" --out "$OUT"
fi

mkdir -p "$RUN_DIR" "$SCRATCH_ROOT" "$(dirname "$OUT")"

export ACOS_FONT_DIR="${ACOS_FONT_DIR:-/home/Qui/Projects/acos/.cache/boot/android-34/fonts}"
unset ACOS_BOOT_DIR

echo "acos-bench: start $STARTED_UTC"
echo "acos-bench: groups [$(echo "$PHASE_GROUPS" | tr '\n' ' ')] reps=$REPS latency_reps=$LATENCY_REPS p11_reps=$P11_REPS animations=$ANIMATIONS task=$TASK"
echo "acos-bench: run dir $RUN_DIR"

# --- build (release only) ---------------------------------------------------
if [ "$BUILD" -eq 1 ] || [ ! -x "$BIN" ]; then
    echo "acos-bench: building release (-p acos-cli -p acos-art) into $TARGET_DIR"
    CARGO_TARGET_DIR="$TARGET_DIR" CARGO_PROFILE_DEV_DEBUG=0 CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-6}" \
        tools/guarded.sh --mem 10G --time 1800 -- \
        cargo build --release --offline -p acos-cli -p acos-art || {
            echo "acos-bench: build failed" >&2; exit 2; }
fi
[ -x "$BIN" ] || { echo "acos-bench: missing $BIN" >&2; exit 2; }
echo "acos-bench: binary $BIN sha256 $(sha256sum "$BIN" | cut -d' ' -f1)"

phase_time() {
    case "$1" in
        latency) echo 600 ;;
        memory) echo 600 ;;
        loop1) echo 240 ;;
        loop5) echo 300 ;;
        cold) echo 300 ;;
        p11) echo 480 ;;
        scenarios) echo 480 ;;
        *) echo 300 ;;
    esac
}
phase_mem() { [ "$1" = memory ] && echo "$MEM_PHASE_MEM" || echo "$OTHER_MEM"; }

FAILED=0
for rep in $(seq 1 "$REPS"); do
    repdir="$RUN_DIR/rep-$rep"
    mkdir -p "$repdir"
    echo "{\"rep\": $rep, \"started_utc\": \"$(date -u +%FT%TZ)\", \"groups\": {}}" > "$repdir/meta.json"
    while IFS= read -r group; do
        [ -z "$group" ] && continue
        gname=${group%%:*}; rest=${group#*:}
        gphases=${rest%%:*}; rest=${rest#*:}
        gmem=${rest%%:*}; gtime=${rest#*:}
        scratch="$SCRATCH_ROOT/r${rep}-${gname}"
        rm -rf "$scratch"; mkdir -p "$scratch"
        t0=$(date +%s)
        echo "acos-bench: rep $rep/$REPS group $gname [$gphases] mem $gmem time ${gtime}s"
        tools/art-runtime/run-exclusive.sh --task "$TASK" -- \
            tools/guarded.sh --mem "$gmem" --time "$gtime" -- \
            python3 "$PY" --bundle "$gphases" --acos "$BIN" \
            --out-dir "$repdir" --scratch-root "$scratch" \
            --reps "$LATENCY_REPS" --p11-reps "$P11_REPS" --loops "$LOOPS" \
            --animations "$ANIMATIONS"
        rc=$?
        t1=$(date +%s)
        wall=$((t1 - t0))
        queue=$(grep "^task=$TASK " /home/Qui/Projects/acos/.cache/art-locks/runs.log 2>/dev/null | tail -1 | sed -n 's/.*queue_wait_s=\([0-9]*\).*/\1/p')
        python3 - "$repdir/meta.json" "$gname" "$gphases" "$wall" "${queue:-}" "$rc" <<'PYM'
import json, sys
path, name, phases, wall, queue, rc = sys.argv[1:7]
try:
    doc = json.load(open(path))
except (OSError, ValueError):
    doc = {}
doc.setdefault("groups", {})[name] = {
    "name": name,
    "phases": [p for p in phases.split(",") if p],
    "wall_s": int(wall),
    "queue_wait_s": int(queue) if queue else None,
    "rc": int(rc),
}
json.dump(doc, open(path, "w"), indent=1)
PYM
        if [ "$rc" -ne 0 ]; then
            echo "acos-bench: group $gname rep $rep FAILED rc=$rc" >&2
            FAILED=1
        fi
        if [ "$KEEP" -eq 0 ]; then rm -rf "$scratch"; fi
    done <<EOF
$PHASE_GROUPS
EOF
done

# --- merge ------------------------------------------------------------------
merge_args=(--merge --run-dir "$RUN_DIR" --acos "$BIN" --out "$OUT" --markdown "$MD"
            --mode "$([ "$QUICK" -eq 1 ] && echo quick || echo full)")
if [ "$ACCEPT" -eq 1 ]; then
    merge_args+=(--accept-baseline "$BASELINE")
fi
python3 "$PY" "${merge_args[@]}"
merge_rc=$?

if [ -n "$COMPARE" ]; then
    python3 "$PY" --compare "$COMPARE" --from-run "$OUT" --out "$OUT"
    cmp_rc=$?
else
    cmp_rc=0
fi

echo "acos-bench: baseline JSON $OUT"
echo "acos-bench: baseline MD   $MD"
[ "$ACCEPT" -eq 1 ] && echo "acos-bench: accepted $BASELINE"
echo "acos-bench: end $(date -u +%FT%TZ)"

if [ "$merge_rc" -ne 0 ]; then
    exit 1
fi
if [ "$FAILED" -ne 0 ]; then
    echo "acos-bench: at least one group reported rc!=0 (see above); the merged " \
         "baseline gate is the authority and it passed" >&2
fi
exit "$cmp_rc"
