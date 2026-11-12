#!/usr/bin/env bash
#
# bench-agent-loop.sh -- W49: what an agent pays per decision through `acos serve`.
#
# The daemon/MCP path an agent actually uses is driven over its real transport
# (newline JSON on a Unix socket) with the fixed script from ADVISOR item 9:
#
#     launch -> tree -> tap -> tree -> type -> key -> screenshot -> assert -> logs -> reset
#
# on two real apps (the counter fixture and form-validation), twice per app.
# Per-verb p50/p95 (>= 20 samples each), round-trips per task, bytes returned
# (tree JSON, PNG) and the provenance block come from tools/benchlib.py.
#
# Gates (each reported PASS / FAIL / NOT_MEASURED, never fudged):
#   * every verb p95 < 500 ms warm, except launch and reset;
#   * the counter tree payload <= 64 KiB;
#   * round-trips per task <= 12;
#   * every task reaches a verdict (a new gate: a loop that cannot verify
#     anything is not a fast loop).
#
# Release only. The binary is read from
#   ${CARGO_TARGET_DIR:-.cache/w49-target}/release/acos
# and the harness refuses a debug path. A missing binary is built with
# `cargo build --release -p acos-cli` (inside the caller's guard).
#
# Usage:
#   tools/art-runtime/run-exclusive.sh --task W49 -- \
#     tools/guarded.sh --mem 8G --time 900 -- \
#     tools/art-runtime/bench-agent-loop.sh [--loops 12] [--runs 2] [--strict]
#
# Options:
#   --loops N       iterations per run (default 12; one warm-up per run)
#   --runs N        runs per app (default 2) => 2*(12-1)=22 samples per verb
#   --apps LIST     comma-separated (default counter,form-validation)
#   --target-dir D  CARGO_TARGET_DIR (default .cache/w49-target)
#   --out FILE      result JSON (default results/bench/agent-loop-<UTC>.json)
#   --markdown FILE table (default results/art-runtime/W49.md)
#   --build         force a release build
#   --host-gate-wait S  wait up to S s for a clean host (default 600)
#   --settle MODE   P1 settle mode for tap/type/key: strict (default) or fast
#   --strict        exit 1 when any gate fails (default: report only)
#   --keep-scratch  keep PNGs/runtime dir (default: pruned after the JSON)
#
# Scratch: .cache/bench-agent-loop/<UTC>/ (never /tmp). Removed after the JSON
# is written unless --keep-scratch.
set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT"

UTC="$(date -u +%Y%m%dT%H%M%SZ)"
STARTED_UTC="$(date -u +%FT%TZ)"
echo "bench-agent-loop: start UTC $STARTED_UTC"

LOOPS=12
RUNS=2
APPS="counter,form-validation"
TARGET_DIR="${CARGO_TARGET_DIR:-$ROOT/.cache/w49-target}"
OUT="$ROOT/results/bench/agent-loop-$UTC.json"
MARKDOWN="$ROOT/results/art-runtime/W49.md"
BUILD=0
STRICT=0
HOST_WAIT=600
KEEP=0
SETTLE=strict

while [ $# -gt 0 ]; do
    case "$1" in
        --loops) LOOPS="$2"; shift 2 ;;
        --runs) RUNS="$2"; shift 2 ;;
        --apps) APPS="$2"; shift 2 ;;
        --target-dir) TARGET_DIR="$2"; shift 2 ;;
        --out) OUT="$2"; shift 2 ;;
        --markdown) MARKDOWN="$2"; shift 2 ;;
        --build) BUILD=1; shift ;;
        --host-gate-wait) HOST_WAIT="$2"; shift 2 ;;
        --settle) SETTLE="$2"; shift 2 ;;
        --strict) STRICT=1; shift ;;
        --keep-scratch) KEEP=1; shift ;;
        -h|--help) sed -n '2,45p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'; exit 0 ;;
        *) echo "bench-agent-loop: unknown argument $1" >&2; exit 2 ;;
    esac
done

SCRATCH="$ROOT/.cache/bench-agent-loop/$UTC"
rm -rf "$SCRATCH"; mkdir -p "$SCRATCH/screens" "$SCRATCH/runtime" "$SCRATCH/cache"
SOCKET="$SCRATCH/serve.sock"
SERVE_LOG="$SCRATCH/serve.log"
BINARY="$TARGET_DIR/release/acos"

# --- 45-minute stamp (the task's hard output requirement) -------------------
(
    sleep 2700
    echo "bench-agent-loop: 45-minute stamp UTC $(date -u +%FT%TZ)"
) &
STAMP_PID=$!
cleanup() {
    local rc=$?
    kill "$STAMP_PID" 2>/dev/null || true
    if [ -n "${SERVE_PID:-}" ] && kill -0 "$SERVE_PID" 2>/dev/null; then
        kill -TERM "$SERVE_PID" 2>/dev/null || true
        for _ in $(seq 1 50); do
            kill -0 "$SERVE_PID" 2>/dev/null || break
            sleep 0.1
        done
        kill -KILL "$SERVE_PID" 2>/dev/null || true
        wait "$SERVE_PID" 2>/dev/null || true
    fi
    if [ "$rc" -ne 0 ]; then
        echo "bench-agent-loop: FAILED rc=$rc; scratch kept at $SCRATCH" >&2
        tail -20 "$SERVE_LOG" 2>/dev/null >&2 || true
    elif [ "$KEEP" -eq 1 ]; then
        echo "bench-agent-loop: scratch kept at $SCRATCH (--keep-scratch)"
    else
        # The JSON has been written; per-run PNGs/runtime state are not evidence.
        rm -rf "$SCRATCH"
    fi
    echo "bench-agent-loop: end UTC $(date -u +%FT%TZ)"
    exit "$rc"
}
trap cleanup EXIT INT TERM

# --- release binary ---------------------------------------------------------
case "$BINARY" in
    */release/acos) ;;
    *) echo "bench-agent-loop: refusing non-release binary path $BINARY" >&2; exit 2 ;;
esac
if [ "$BUILD" -eq 1 ] || [ ! -x "$BINARY" ]; then
    echo "bench-agent-loop: cargo build --release -p acos-cli (CARGO_TARGET_DIR=$TARGET_DIR)"
    CARGO_TARGET_DIR="$TARGET_DIR" CARGO_PROFILE_DEV_DEBUG=0 \
        cargo build --release -p acos-cli || exit 2
fi
[ -x "$BINARY" ] || { echo "bench-agent-loop: missing $BINARY" >&2; exit 2; }
echo "bench-agent-loop: binary $BINARY sha256 $(sha256sum "$BINARY" | cut -d' ' -f1)"

# --- host gate (this process is inside the host-wide ART lock) --------------
# Wait for a clean host window instead of recording a contaminated run. The gate
# itself is not relaxed: a run that cannot get loadavg1 <= 4 or MemAvailable
# >= 30 GiB fails here and is never reported as a measurement.
HOST_DEADLINE=$(( $(date +%s) + HOST_WAIT ))
while :; do
    python3 -I - "$ROOT" <<'PY'
import sys
sys.path.insert(0, sys.argv[1] + "/tools")
import benchlib
g = benchlib.host_gate(sys.argv[1], check_lock=False)
print("bench-agent-loop: host gate ok=%s loadavg1=%.2f mem_available=%.1f GiB%s"
      % (g.get("ok"), g.get("loadavg1") or 0.0, g.get("mem_available_gib") or 0.0,
         "" if g.get("ok") else " reasons=" + "; ".join(g.get("reasons") or [])))
sys.exit(0 if g.get("ok") else 1)
PY
    if [ $? -eq 0 ]; then break; fi
    if [ "$(date +%s)" -ge "$HOST_DEADLINE" ]; then
        echo "bench-agent-loop: host gate refused after ${HOST_WAIT}s wait" >&2
        exit 3
    fi
    echo "bench-agent-loop: host busy; retrying in 15s (UTC $(date -u +%FT%TZ))"
    sleep 15
done

# --- daemon -----------------------------------------------------------------
echo "bench-agent-loop: starting acos serve on $SOCKET"
ACOS_RUNTIME_DIR="$SCRATCH/runtime" ACOS_CACHE_DIR="$SCRATCH/cache" \
    "$BINARY" serve --socket "$SOCKET" --idle-timeout 600 --max-sessions 8 \
    >"$SCRATCH/serve.stdout" 2>"$SERVE_LOG" &
SERVE_PID=$!

for _ in $(seq 1 200); do
    [ -S "$SOCKET" ] && break
    kill -0 "$SERVE_PID" 2>/dev/null || { echo "bench-agent-loop: serve died" >&2; exit 3; }
    sleep 0.05
done
[ -S "$SOCKET" ] || { echo "bench-agent-loop: socket never appeared" >&2; exit 3; }
python3 -I - "$SOCKET" <<'PY' || exit 3
import json, socket, sys
s = socket.socket(socket.AF_UNIX); s.settimeout(10); s.connect(sys.argv[1])
s.sendall(b'{"id":1,"tool":"ping","args":{}}\n')
print("bench-agent-loop: ping " + s.makefile("rb").readline().decode().strip())
PY

# --- drive the loop ---------------------------------------------------------
echo "bench-agent-loop: loops=$LOOPS runs=$RUNS apps=$APPS -> $OUT"
ACOS_BENCH_BINARY="$BINARY" python3 -I "$ROOT/tools/art-runtime/agent_loop.py" \
    --socket "$SOCKET" --out "$OUT" --markdown "$MARKDOWN" \
    --scratch "$SCRATCH" --apps "$APPS" --loops "$LOOPS" --runs "$RUNS" --settle "$SETTLE" \
    $([ "$STRICT" -eq 1 ] && echo --strict) \
    $([ "${ALLOW_DIRTY:-0}" = "1" ] && echo --allow-dirty)
RC=$?

# --- prune --------------------------------------------------------------
if [ -s "$OUT" ]; then
    echo "bench-agent-loop: result $OUT ($(stat -c '%s' "$OUT") bytes)"
    echo "bench-agent-loop: table   $MARKDOWN"
    python3 -I - "$OUT" <<'PY'
import json, sys
d = json.load(open(sys.argv[1]))
g = d["agent_loop"]["gates"]
print("bench-agent-loop: overall gate %s" % g.get("overall"))
for k, v in g.items():
    if k != "overall":
        print("  %-40s %s" % (k, v))
print("bench-agent-loop: gaps %d" % len(d["agent_loop"]["gaps"]))
PY
fi
exit "$RC"
