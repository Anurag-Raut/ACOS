#!/usr/bin/env bash
#
# bench-d2.sh -- W33: the D2 table (M1-M11) measured on the real ART engine,
# release binaries only, with the W29 statistics/noise/verdict rules
# (tools/benchlib.py; BENCHMARKS.md section 11) and the ADVISOR item-1 design
# (results/art-runtime/ADVISOR-benchmarks-top10.md global decisions).
#
#   tools/art-runtime/bench-d2.sh [--samples 20] [--mem-reps 5]
#                                 [--cohorts "1 5 10 20"] [--suite 12]
#                                 [--target-dir .cache/w33-target] [--no-build]
#                                 [--bin-dir DIR] [--out DIR] [--quick] [--analyze DIR]
#
#   tools/art-runtime/bench-d2.sh --mode warm [--warm-reps 5] [--warm-settle 60]
#                                 [--warm-samples 20] [--warm-resets 10]
#                                 [--host-gate-wait 600] [--analyze-warm DIR]
#
# MODES
#   cold (default)  the original one-shot rows: `acos verify --engine art` once per
#                   task. M1/M2/M4/M6/M7 cannot be measured here (cold process) and
#                   are reported NOT_MEASURED / NOT_COMPARABLE.
#   warm (W57)      M1, M2, M5, M6 and M7 on a HELD `acos serve` ART session, the
#                   same state legacy measured: one `acos serve` daemon per rep,
#                   cgroup anon+shmem (job + guest cgroups) 60 s after the first
#                   frame, warm screenshot/tree/reset requests over the real
#                   socket (`tools/art-runtime/d2_warm.py`, which imports
#                   `agent_loop.py`'s Daemon). One rep = one run-exclusive.sh
#                   acquisition. `--analyze-warm DIR` re-derives the result table
#                   from an existing directory without running ART.
#
# Output (default results/bench/):
#   d2-<UTCstamp>.json   every row, sample, statistic, provenance, host load
#   d2-<UTCstamp>.md     the Markdown table
#   d2-warm-<UTCstamp>.json/.md   the warm-mode rows (W57)
#
# Rules enforced here
#   * RELEASE ONLY: a debug (or unknown-profile) binary is refused before any
#     ART guest starts (exit 5). `--bin-dir target/debug` demonstrates it.
#   * ONE ART RUN PER run-exclusive.sh ACQUISITION: every latency sample, every
#     memory-cohort rep and every suite task is a separate
#     `run-exclusive.sh --task W33 -- guarded.sh --mem 8G --time 900 -- ...`
#     invocation. The host-wide lock is never held across reps, so other agents'
#     ART runs interleave (their queue waits and ours are recorded in
#     .cache/art-locks/runs.log).
#   * Per rep, host loadavg/MemAvailable are recorded; a sample whose host gate
#     failed (loadavg1 > 4 or MemAvailable < 30 GiB) is marked contaminated and
#     the row can never PASS (benchlib spread gate + contamination reason).
#   * No metric is estimated. A row that needs a warm/held ART session ACOS does
#     not have (60 s settle, warm worker start, session reset) is NOT_MEASURED
#     with the reason spelled out.
#
# Metric states: `cold-process` = no ACOS process alive, page cache warm;
# `cold` = the legacy cold state category; `warm` = a session/pool already up.
# Metric types are stated per row; a type/state mismatch is NOT_COMPARABLE
# (never a win). See BENCHMARKS.md section 11 and tools/benchlib.py.
#
# Host disk (W48; on 2026-11-11/12 this script filled the disk and kept 36 GB of
# per-rep artifacts):
#   * PREFLIGHT: before the build and again before the first ART run, the free
#     space on the output filesystem must be >= ACOS_BENCH_MIN_FREE_GB (default
#     40). Too little space exits 6 with a `HOST_DISK_FULL:` message; a run that
#     cannot write its artifacts must not start.
#   * PRUNING: after each rep's row is recorded, that rep's bulky artifacts
#     (frames/, tree/, logs/, live frames, PNGs) are deleted. verdict.json,
#     resources.json, steps/, repro.*, summary.json, run.json and stdout stay.
#     ACOS_BENCH_KEEP_ARTIFACTS=1 disables the pruning.
set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT" || exit 1

usage() { awk 'NR > 1 && /^set -uo/ { exit } NR > 1 { sub(/^# ?/, ""); print }' "${BASH_SOURCE[0]}"; }

SAMPLES=20
MEM_REPS=5
COHORTS="1 5 10 20"
SUITE_MAX=12
TARGET_DIR=".cache/w33-target"
BIN_DIR=""
OUT=""
NO_BUILD=0
QUICK=0
ANALYZE=""
MODE="cold"
WARM_REPS=5
WARM_SETTLE=60
WARM_SAMPLES=20
WARM_RESETS=10
WARM_LATENCY=1
HOST_WAIT=600
ANALYZE_WARM=""

while [ $# -gt 0 ]; do
    case "$1" in
        --samples)    SAMPLES="$2"; shift 2 ;;
        --mem-reps)   MEM_REPS="$2"; shift 2 ;;
        --cohorts)    COHORTS="$2"; shift 2 ;;
        --suite)      SUITE_MAX="$2"; shift 2 ;;
        --target-dir) TARGET_DIR="$2"; shift 2 ;;
        --bin-dir)    BIN_DIR="$2"; shift 2 ;;
        --out)        OUT="$2"; shift 2 ;;
        --no-build)   NO_BUILD=1; shift ;;
        --analyze)    ANALYZE="$2"; shift 2 ;;
        --mode)       MODE="$2"; shift 2 ;;
        --warm-reps)  WARM_REPS="$2"; shift 2 ;;
        --warm-settle) WARM_SETTLE="$2"; shift 2 ;;
        --warm-samples) WARM_SAMPLES="$2"; shift 2 ;;
        --warm-resets) WARM_RESETS="$2"; shift 2 ;;
        --host-gate-wait) HOST_WAIT="$2"; shift 2 ;;
        --analyze-warm) ANALYZE_WARM="$2"; shift 2 ;;
        --no-warm-latency) WARM_LATENCY=0; shift ;;
        --quick)      QUICK=1; SAMPLES=2; MEM_REPS=1; COHORTS="1 5"; SUITE_MAX=2; shift ;;
        -h|--help)    usage; exit 0 ;;
        *) echo "bench-d2: unknown argument: $1" >&2; exit 2 ;;
    esac
done

case "$MODE" in
    cold|warm) ;;
    *) echo "bench-d2: unknown --mode $MODE (cold|warm)" >&2; exit 2 ;;
esac

STAMP="$(date -u +%Y%m%dT%H%M%SZ)"
mkdir -p "$ROOT/results/bench"

if [ -n "$ANALYZE" ]; then
    [ -f "$ANALYZE/summary.json" ] || { echo "bench-d2: no $ANALYZE/summary.json" >&2; exit 2; }
    [ -f "$ANALYZE/driver.py" ] || { echo "bench-d2: no $ANALYZE/driver.py" >&2; exit 2; }
    exec python3 -I "$ANALYZE/driver.py" --analyze "$ANALYZE"
fi

[ -n "$OUT" ] || OUT="$ROOT/.cache/bench-d2/$STAMP"
mkdir -p "$OUT"

# ---- warm mode re-derive (W57): no ART, no build, no host gate ----
# Aggregating an existing warm outdir only re-runs the statistics/verdict code, so
# it must work on a host that is busy and without a release binary present.
if [ "$MODE" = "warm" ] && [ -n "$ANALYZE_WARM" ]; then
    set -- python3 -I "$ROOT/tools/art-runtime/d2_warm.py" --analyze "$ANALYZE_WARM"
    [ -n "${WARM_ANALYZE_OUT:-}" ] && set -- "$@" --out "$WARM_ANALYZE_OUT"
    [ -n "${WARM_ANALYZE_MD:-}" ] && set -- "$@" --markdown "$WARM_ANALYZE_MD"
    exec "$@"
fi

# ---- W48: host disk preflight -------------------------------------------
# The disk filled to 100% during the 11-12 benchmark window and a run with no
# space left produces misleading verdicts. Free space is a host condition: check
# it before anything is written and refuse clearly (never a silent partial run).
ACOS_BENCH_MIN_FREE_GB="${ACOS_BENCH_MIN_FREE_GB:-40}"
disk_preflight() {
    local avail
    avail="$(df -Pk "$OUT" 2>/dev/null | awk 'NR==2 {printf "%d", $4/1048576}')"
    if [ -z "$avail" ]; then
        echo "bench-d2: warning: free space on $OUT is unreadable; continuing" >&2
        return 0
    fi
    if [ "$avail" -lt "$ACOS_BENCH_MIN_FREE_GB" ]; then
        echo "HOST_DISK_FULL: bench-d2 refuses to run: ${avail} GiB free on the filesystem holding" >&2
        echo "  $OUT, below ACOS_BENCH_MIN_FREE_GB=${ACOS_BENCH_MIN_FREE_GB} GiB." >&2
        echo "  Free space (or lower ACOS_BENCH_MIN_FREE_GB deliberately) and retry." >&2
        return 1
    fi
    echo "bench-d2: disk preflight ok: ${avail} GiB free on $OUT (>= ${ACOS_BENCH_MIN_FREE_GB} GiB)" >&2
}
disk_preflight || exit 6

case "$TARGET_DIR" in
    /*) TARGET_ROOT="$TARGET_DIR" ;;
    *)  TARGET_ROOT="$ROOT/$TARGET_DIR" ;;
esac

# ---- release build (guarded); --no-build reuses the existing binaries ----
if [ -z "$BIN_DIR" ]; then
    BIN_DIR="$TARGET_ROOT/release"
    if [ "$NO_BUILD" -eq 0 ]; then
        echo "bench-d2: building release into $TARGET_DIR (guarded)" >&2
        if ! CARGO_TARGET_DIR="$TARGET_DIR" "$ROOT/tools/guarded.sh" --mem 12G --time 900 -- \
                cargo build --release -p acos-cli -p acos-art >"$OUT/build.log" 2>&1; then
            echo "bench-d2: release build failed; see $OUT/build.log" >&2
            exit 3
        fi
    fi
fi
ACOS="$BIN_DIR/acos"
ACOS_ART="$BIN_DIR/acos-art"
for b in "$ACOS" "$ACOS_ART"; do
    [ -x "$b" ] || { echo "bench-d2: $b missing (build it or pass --target-dir/--bin-dir)" >&2; exit 3; }
done

# ---- RELEASE ONLY: refuse debug/unknown before any ART guest starts ----
if ! python3 -I - "$ROOT" "$ACOS" "$ACOS_ART" <<'PY'
import json
import sys
sys.path.insert(0, sys.argv[1] + "/tools")
import benchlib
bins = [benchlib.binary_provenance(sys.argv[2]), benchlib.binary_provenance(sys.argv[3])]
for b in bins:
    print("bench-d2: %s profile=%s sha256=%s verdict_eligible=%s"
          % (b["path"], b["profile"], (b["sha256"] or "")[:16], b["verdict_eligible"]),
          file=sys.stderr)
bad = [b for b in bins if not b["verdict_eligible"]]
if bad:
    print("bench-d2: REFUSING: %s" % (bad[0]["not_eligible_reason"]), file=sys.stderr)
    print("bench-d2: a debug/unknown build can never produce a D2 verdict; "
          "build release with tools/art-runtime/bench-release.sh.", file=sys.stderr)
    sys.exit(5)
PY
then
    echo "bench-d2: refused before any ART run (exit 5)" >&2
    exit 5
fi

# ---- host gate (loadavg1 <= 4, MemAvailable >= 30 GiB; lock taken per run) ----
if ! python3 -I - "$ROOT" <<'PY'
import sys
sys.path.insert(0, sys.argv[1] + "/tools")
import benchlib
g = benchlib.host_gate(sys.argv[1], check_lock=False)
print("bench-d2: host gate %s (loadavg1 %s, MemAvailable %s GiB)"
      % ("ok" if g["ok"] else "REFUSED", g["loadavg1"], g["mem_available_gib"]),
      file=sys.stderr)
if not g["ok"]:
    for r in g["reasons"]:
        print("bench-d2:   " + r, file=sys.stderr)
    sys.exit(1)
PY
then
    exit 4
fi

disk_preflight || exit 6
echo "bench-d2: mode=$MODE samples=$SAMPLES mem-reps=$MEM_REPS cohorts='$COHORTS' suite=$SUITE_MAX" >&2

# ---- warm mode (W57): M1/M2/M5/M6/M7 on a held `acos serve` ART session ----
# The cold driver below measures one-shot `acos verify` runs, which cannot reach a
# 60 s settle or a warm request. Warm mode instead runs the W57 harness
# (tools/art-runtime/d2_warm.py) once per rep inside its own
# run-exclusive.sh + guarded.sh acquisition, so the host-wide ART lock is never
# held across reps, and then aggregates every rep with benchlib's verdict rule.
if [ "$MODE" = "warm" ]; then
    WARM_DRIVER="$ROOT/tools/art-runtime/d2_warm.py"
    WARM_OUT="$OUT/warm"
    mkdir -p "$WARM_OUT"
    WARM_TASK="${ACOS_BENCH_TASK:-W57}"
    echo "bench-d2: warm mode: reps=$WARM_REPS settle=${WARM_SETTLE}s samples=$WARM_SAMPLES resets=$WARM_RESETS" >&2
    echo "bench-d2: warm reps -> $WARM_OUT (one run-exclusive.sh acquisition per rep)" >&2

    # Wait for a clean host window instead of recording a contaminated run: the
    # gate itself is not relaxed (loadavg1 <= 4, MemAvailable >= 30 GiB).
    HOST_DEADLINE=$(( $(date +%s) + HOST_WAIT ))
    while :; do
        if python3 -I - "$ROOT" <<'PY'
import sys
sys.path.insert(0, sys.argv[1] + "/tools")
import benchlib
g = benchlib.host_gate(sys.argv[1], check_lock=False)
print("bench-d2: warm host gate ok=%s loadavg1=%s mem_available=%s GiB"
      % (g.get("ok"), g.get("loadavg1"), g.get("mem_available_gib")), file=sys.stderr)
if not g.get("ok"):
    for r in g.get("reasons") or []:
        print("bench-d2:   " + r, file=sys.stderr)
sys.exit(0 if g.get("ok") else 1)
PY
        then break; fi
        if [ "$(date +%s)" -ge "$HOST_DEADLINE" ]; then
            echo "bench-d2: warm host gate refused after ${HOST_WAIT}s wait" >&2
            exit 4
        fi
        echo "bench-d2: host busy; waiting 15s (UTC $(date -u +%FT%TZ))" >&2
        sleep 15
    done

    warm_rep() { # warm_rep N REP [EXTRA...]
        local n="$1" rep="$2"; shift 2
        echo "bench-d2: warm rep N=$n rep=$rep (UTC $(date -u +%FT%TZ))" >&2
        "$ROOT/tools/art-runtime/run-exclusive.sh" --task "$WARM_TASK" -- \
            "$ROOT/tools/guarded.sh" --mem 16G --time 1500 -- \
            python3 -I "$WARM_DRIVER" --acos "$ACOS" --phase mem --n "$n" --rep "$rep" \
                --outdir "$WARM_OUT" --settle "$WARM_SETTLE" \
                --samples "$WARM_SAMPLES" --resets "$WARM_RESETS" "$@"
        return $?
    }

    WARM_RC=0
    for rep in $(seq 1 "$WARM_REPS"); do
        if [ "$rep" -eq 1 ] && [ "$WARM_LATENCY" -eq 1 ]; then
            warm_rep 1 "$rep" --with-latency || WARM_RC=1
        else
            warm_rep 1 "$rep" || WARM_RC=1
        fi
    done
    if [ "$WARM_LATENCY" -eq 1 ] && [ "$WARM_REPS" -lt 2 ]; then
        warm_rep 1 0 --with-latency || WARM_RC=1
    fi
    for rep in $(seq 1 "$WARM_REPS"); do
        warm_rep 5 "$rep" || WARM_RC=1
    done
    echo "bench-d2: aggregating warm reps (UTC $(date -u +%FT%TZ))" >&2
    python3 -I "$WARM_DRIVER" --analyze "$WARM_OUT" || exit 1
    echo "bench-d2: warm mode finished rc=$WARM_RC (UTC $(date -u +%FT%TZ))" >&2
    exit "$WARM_RC"
fi

# ---- the measurement driver (embedded; one ART run per lock acquisition) ----
cat >"$OUT/driver.py" <<'PY'
import datetime
import json
import math
import os
import re
import shutil
import subprocess
import sys
import threading
import time

ROOT, OUT, ACOS, ACOS_ART = sys.argv[1:5]
SAMPLES = int(sys.argv[5]); MEM_REPS = int(sys.argv[6])
COHORTS = [int(x) for x in sys.argv[7].split()]
SUITE_MAX = int(sys.argv[8]); QUICK = sys.argv[9] == "1"

sys.path.insert(0, os.path.join(ROOT, "tools"))
import benchlib  # noqa: E402

RUNEX = os.path.join(ROOT, "tools/art-runtime/run-exclusive.sh")
GUARD = os.path.join(ROOT, "tools/guarded.sh")
APK = os.path.join(ROOT, "fixtures/build/apps/counter.apk")
SCEN = os.path.join(ROOT, "scenarios/v2-art-counter.yaml")
ARTI = os.path.join(OUT, "artifacts")
MiB = 1024.0 * 1024.0
STAMP = datetime.datetime.now(datetime.timezone.utc).strftime("%Y%m%dT%H%M%SZ")
RESULT_JSON = os.path.join(ROOT, "results/bench/d2-%s.json" % STAMP)
RESULT_MD = os.path.join(ROOT, "results/bench/d2-%s.md" % STAMP)
ALLOWED = {"PASS", "TIE", "FAIL", "NOISY", "NOT_COMPARABLE", "NOT_MEASURED",
           "INSUFFICIENT"}

# ---- W48: prune a rep's bulky artifacts once its row has been recorded ----
# 36 GB of frames/tree/logs survived the 11-12 window and filled the disk. The
# evidence that makes a row checkable (verdict.json, resources.json, steps/,
# repro.*) stays; the pixels and dumps go as soon as the sample is parsed.
KEEP_ARTIFACTS = os.environ.get("ACOS_BENCH_KEEP_ARTIFACTS", "0") == "1"
BULKY_DIRS = {"frames", "tree", "logs", "art-frames", "live-frames",
              "screenshots", "session", "session-root"}
# These two are tiny and are the *record* of the run: session-exit.json carries
# the exit/signal/stage facts and the effective heap configuration (W48), and
# live-session.json carries the interactive session's own evidence. Keeping them
# is what lets a pruned benchmark row still name its heap configuration.
SESSION_RECORD_DIRS = {"art-frames", "live-frames"}
SESSION_RECORDS = {"session-exit.json", "live-session.json"}


def _prune_dir(target):
    """Delete a bulky directory, keeping the session records when it holds them."""
    if os.path.basename(target) in SESSION_RECORD_DIRS:
        for name in os.listdir(target):
            if name in SESSION_RECORDS:
                continue
            path = os.path.join(target, name)
            if os.path.isdir(path):
                shutil.rmtree(path, ignore_errors=True)
            else:
                try:
                    os.remove(path)
                except OSError:
                    pass
    else:
        shutil.rmtree(target, ignore_errors=True)


def dir_bytes(path):
    total = 0
    for base, _dirs, files in os.walk(path):
        for f in files:
            try:
                total += os.path.getsize(os.path.join(base, f))
            except OSError:
                pass
    return total


def prune_artifacts(path, tag):
    """Delete the bulky per-rep artifacts after the row is in the run record.

    Kept: verdict/resources/step/repro/events JSON, the two session records
    (session-exit.json, live-session.json) and the host record (run.json,
    stdout.raw). Deleted: every frames/, tree/, logs/ directory, the live frame
    buffers and every PNG, at any depth (cohort reps nest their per-session
    artifacts dirs). ACOS_BENCH_KEEP_ARTIFACTS=1 disables this.
    """
    if KEEP_ARTIFACTS or not os.path.isdir(path):
        return
    before = dir_bytes(path)
    for base, dirs, files in os.walk(path, topdown=True):
        for d in list(dirs):
            if d in BULKY_DIRS:
                _prune_dir(os.path.join(base, d))
                dirs.remove(d)
        for f in files:
            drop = f.endswith(".png") or (f.startswith("tree-") and f.endswith(".json"))
            if drop:
                try:
                    os.remove(os.path.join(base, f))
                except OSError:
                    pass
    freed = before - dir_bytes(path)
    print("bench-d2: pruned %s: %.1f MiB freed (frames/tree/logs/live frames/PNGs "
          "removed; verdict/resources/steps/repro/session-exit kept; "
          "ACOS_BENCH_KEEP_ARTIFACTS=1 disables)" % (tag, freed / MiB), file=sys.stderr)


def utc():
    return datetime.datetime.now(datetime.timezone.utc).isoformat(timespec="seconds")


def host_now():
    return {**benchlib.read_meminfo(), **benchlib.read_loadavg(),
            "utc": utc()}


def host_ok(h):
    return (h.get("loadavg1") is not None and h["loadavg1"] <= 4.0
            and h.get("mem_available_gib") is not None
            and h["mem_available_gib"] >= 30.0)


def art_run(cmd, tag):
    """ONE ART run = ONE run-exclusive.sh acquisition (lock never held across reps)."""
    adir = os.path.join(ARTI, tag)
    os.makedirs(adir, exist_ok=True)
    before = host_now()
    task = os.environ.get("ACOS_BENCH_TASK", "W33")
    argv = [RUNEX, "--task", task, "--", GUARD, "--mem", "8G", "--time", "900", "--"] + cmd
    env = dict(os.environ, ACOS_BENCH_PROFILE="release")
    t0 = time.time()
    try:
        p = subprocess.run(argv, cwd=ROOT, env=env, capture_output=True, text=True,
                           timeout=1000)
        rc, out, err = p.returncode, p.stdout, p.stderr
    except subprocess.SubprocessError as exc:
        rc, out, err = 124, "", "subprocess failed: %s" % exc
    wall = time.time() - t0
    after = host_now()
    rec = {"tag": tag, "rc": rc, "wall_s": round(wall, 3),
           "host_before": before, "host_after": after,
           "contaminated": not (host_ok(before) and host_ok(after)),
           "stderr_tail": err[-2000:]}
    with open(os.path.join(adir, "run.json"), "w") as f:
        json.dump({**rec, "argv": argv, "stdout_bytes": len(out)}, f, indent=2)
    with open(os.path.join(adir, "stdout.raw"), "w") as f:
        f.write(out)
    return rec, out


def verify_sample(tag):
    adir = os.path.join(ARTI, tag, "verify")
    rec, out = art_run([ACOS, "verify", "--scenario", SCEN, "--apk", APK,
                        "--engine", "art", "--artifacts", adir, "--json"], tag)
    sample = {"tag": tag, "run": rec, "parse_ok": False}
    try:
        doc = json.loads(out)
    except ValueError:
        return sample
    v = (doc.get("result") or {}).get("verdict") or {}
    steps = v.get("steps") or []
    cum = 0
    first_frame = launch = tree_ms = capture_ms = reset_ms = None
    taps = []
    for i, s in enumerate(steps):
        d = float(s.get("duration_ms") or 0.0)
        cum += d
        name = s.get("step")
        if name == "launch":
            launch = d
        if first_frame is None and (s.get("frame_seq") or 0) >= 1:
            first_frame = cum
        if name == "assert_text" and tree_ms is None:
            tree_ms = d
        if name == "capture":
            capture_ms = d
        if name == "tap":
            taps.append({"index": i, "ms": d})
        if len(taps) == 2 and name == "tap" and reset_ms is None:
            nxt = steps[i + 1] if i + 1 < len(steps) else {}
            reset_ms = d + float(nxt.get("duration_ms") or 0.0)
    status = v.get("status") or doc.get("status")
    steps_sum = sum(float(x.get("duration_ms") or 0) for x in steps)
    wall_ms = (v.get("resources") or {}).get("wall_ms")
    overhead = (float(wall_ms) - steps_sum) if wall_ms is not None else 0.0
    sample.update({
        "parse_ok": True,
        "ok": doc.get("ok") is True and status == "pass",
        "status": status, "engine": v.get("engine"),
        "wall_ms": wall_ms,
        "steps_sum_ms": round(steps_sum, 1),
        "timer_overhead_ms": round(overhead, 1),
        "first_frame_engine_timer_ms": first_frame,
        "first_frame_ms_with_timer_overhead": (round(first_frame + max(overhead, 0.0), 1)
                                              if first_frame is not None else None),
        "first_frame_ms": first_frame, "launch_step_ms": launch,
        "tree_text_ms": tree_ms, "capture_ms": capture_ms,
        "reset_app_tap_to_assert_ms": reset_ms,
        "tap_ms": [t["ms"] for t in taps],
        "tap0_ms": taps[0]["ms"] if taps else None,
        "all_steps_pass": bool(steps) and all(s.get("status") == "pass" for s in steps),
        "step_names": [s.get("step") for s in steps],
    })
    return sample


# ---------------------------------------------------------------- cohort child
CHILD_SRC = r'''
import json, os, subprocess, sys, threading, time
cfg = json.load(open(sys.argv[1]))
ROOT, ACOS, APK, SCEN, N, OUT = (cfg["root"], cfg["acos"], cfg["apk"],
                                 cfg["scenario"], int(cfg["n"]), cfg["adir"])
sys.path.insert(0, os.path.join(ROOT, "tools"))
import benchlib

MiB = 1024.0 * 1024.0
def read(p):
    try:
        return open(p).read()
    except OSError:
        return ""
def comm(pid):
    return read("/proc/%d/comm" % pid).strip()
def exe(pid):
    try:
        return os.readlink("/proc/%d/exe" % pid)
    except OSError:
        return ""
def ppid_map():
    m = {}
    for name in os.listdir("/proc"):
        if not name.isdigit():
            continue
        s = read("/proc/%s/stat" % name)
        try:
            ppid = int(s.rsplit(")", 1)[1].split()[1])
        except (IndexError, ValueError):
            continue
        m.setdefault(ppid, []).append(int(name))
    return m
def tree(roots, pm):
    seen, stack = set(), list(roots)
    while stack:
        p = stack.pop()
        if p in seen:
            continue
        seen.add(p)
        stack.extend(pm.get(p, []))
    return seen
def cgpath(pid):
    for line in read("/proc/%d/cgroup" % pid).splitlines():
        if line.startswith("0::"):
            return line[3:].strip()
    return None
def cg_read(rel, name):
    try:
        return int(read(os.path.join("/sys/fs/cgroup", rel.lstrip("/"), name)).strip())
    except (OSError, ValueError):
        return 0
def cg_map(rel, name, keys):
    out = dict.fromkeys(keys, 0)
    for line in read(os.path.join("/sys/fs/cgroup", rel.lstrip("/"), name)).splitlines():
        k, _, v = line.partition(" ")
        if k in out:
            try:
                out[k] = int(v.strip())
            except ValueError:
                pass
    return out

sessions = []
for i in range(N):
    adir = os.path.join(OUT, "s%d" % i)
    os.makedirs(adir, exist_ok=True)
    fh = open(os.path.join(adir, "stdout.json"), "w")
    p = subprocess.Popen([ACOS, "verify", "--scenario", SCEN, "--apk", APK,
                          "--engine", "art", "--artifacts", adir, "--json"],
                         cwd=ROOT, stdin=subprocess.DEVNULL, stdout=fh,
                         stderr=open(os.path.join(adir, "stderr.txt"), "w"),
                         start_new_session=True)
    sessions.append({"i": i, "p": p, "adir": adir, "fh": fh, "samples": []})
t0 = time.time()
stop = threading.Event()
def sampler():
    while not stop.is_set():
        pm = ppid_map(); t = time.time()
        for s in sessions:
            guest = None
            for pid in tree([s["p"].pid], pm):
                if comm(pid) == "dalvikvm64" or exe(pid).endswith("/dalvikvm64"):
                    guest = pid
                    break
            if guest is None:
                continue
            rel = cgpath(guest)
            if not rel:
                continue
            st = cg_map(rel, "memory.stat", ("anon", "shmem"))
            s["samples"].append({"t": t, "current": cg_read(rel, "memory.current"),
                                 "anon": st.get("anon", 0), "shmem": st.get("shmem", 0),
                                 "cgroup": rel})
        time.sleep(0.1)
th = threading.Thread(target=sampler, daemon=True); th.start()
for s in sessions:
    try:
        s["rc"] = s["p"].wait(timeout=600)
    except subprocess.TimeoutExpired:
        s["p"].kill(); s["rc"] = 124
    s["fh"].close()
stop.set(); th.join(timeout=10)
wall = time.time() - t0
per = []
for s in sessions:
    sm = [x for x in s["samples"] if x.get("current")]
    peak = max((x["current"] for x in sm), default=0) / MiB
    last = sm[-1] if sm else {}
    anon_shmem = (last.get("anon", 0) + last.get("shmem", 0)) / MiB if last else None
    gib_min = benchlib.sample_integral([{"t": x["t"], "bytes": x["current"]} for x in sm])
    try:
        doc = json.load(open(os.path.join(s["adir"], "stdout.json")))
        v = (doc.get("result") or {}).get("verdict") or {}
        passed = doc.get("ok") is True and (v.get("status") or doc.get("status")) == "pass"
        wall_ms = (v.get("resources") or {}).get("wall_ms")
    except Exception:
        passed, wall_ms = False, None
    per.append({"i": s["i"], "rc": s["rc"], "pass": passed, "wall_ms": wall_ms,
                "cgroup_peak_mib": round(peak, 2),
                "anon_shmem_last_mib": round(anon_shmem, 2) if anon_shmem is not None else None,
                "gib_min": gib_min, "cgroup_samples": len(sm),
                "cgroup": last.get("cgroup")})
result = {"n": N, "wall_s": round(wall, 3),
          "passed": sum(1 for p in per if p["pass"]), "sessions": per,
          "host": {**benchlib.read_meminfo(), **benchlib.read_loadavg()}}
with open(cfg["result_path"], "w") as f:
    json.dump(result, f, indent=2)
'''


def write_child():
    path = os.path.join(OUT, "cohort_child.py")
    with open(path, "w") as f:
        f.write(CHILD_SRC)
    return path


CHILD = write_child()


def cohort_rep(n, rep):
    adir = os.path.join(OUT, "cohort-N%d-rep%d" % (n, rep))
    os.makedirs(adir, exist_ok=True)
    cfgp = os.path.join(adir, "cfg.json")
    resp = os.path.join(adir, "result.json")
    with open(cfgp, "w") as f:
        json.dump({"root": ROOT, "acos": ACOS, "apk": APK, "scenario": SCEN,
                   "n": n, "adir": adir, "result_path": resp}, f)
    rec, _ = art_run([sys.executable, "-I", CHILD, cfgp], "cohort-N%d-rep%d" % (n, rep))
    try:
        with open(resp) as f:
            res = json.load(f)
    except (OSError, ValueError):
        res = {"n": n, "sessions": [], "passed": 0,
               "error": "cohort result missing (rc=%s)" % rec["rc"]}
    res["run"] = rec
    res["contaminated"] = rec["contaminated"]
    return res


# ---------------------------------------------------------------- stats / rows
def stat_of(vals):
    st = benchlib.stats([v for v in vals if v is not None])
    st["noisy"] = benchlib.is_noisy(st)
    return st


def legacy_stats(value, n=3, spread=None):
    """Legacy published values carry a p50; spread was usually not published.

    spread=None means "not published": the verdict uses the 5 % noise floor and
    the row records `legacy_spread_known: false` so nobody mistakes the 0 for a
    measured spread.
    """
    if value is None:
        return None
    return {"n": int(n), "p50": float(value), "median": float(value),
            "p5": None, "p95": None,
            "spread": (float(spread) if spread is not None else 0.0),
            "spread_pct": (100.0 * float(spread) if spread is not None else 0.0),
            "spread_known": spread is not None}


def decide(acos_stats, legacy_value, *, mtype, legacy_type=None, state,
           legacy_state=None, legacy_n=3, legacy_spread=None, contaminated=0,
           extra_reasons=(), lower_is_better=True):
    b = legacy_stats(legacy_value, legacy_n, legacy_spread)
    v = benchlib.verdict(acos_stats, b, metric_type=mtype,
                         legacy_type=(legacy_type if legacy_type is not None else mtype),
                         state=state, legacy_state=legacy_state,
                         min_samples=3, lower_is_better=lower_is_better)
    for r in extra_reasons:
        if r not in v["reasons"]:
            v["reasons"].append(r)
    if contaminated:
        v["reasons"].append("%d sample(s) recorded a failed host gate "
                            "(loadavg1 > 4 or MemAvailable < 30 GiB) and are excluded "
                            "from the verdict" % contaminated)
        if v["verdict"] == "PASS":
            v["verdict"] = "NOISY"
            v["pass"] = False
            v["reasons"].append("a contaminated run can never PASS")
    return v


def row(mid, name, unit, mtype, state, legacy_value, legacy_unit, legacy_type,
        legacy_state, verdict, acos_value, stats, detail, n=0, extra=None,
        legacy_source="-", legacy_n=3, legacy_spread=None, contaminated=0):
    r = {
        "id": mid, "metric": name, "unit": unit,
        "acos": {"value": acos_value, "metric_type": benchlib.canonical_metric_type(mtype),
                 "state": state, "n": n, "stats": stats, "detail": detail},
        "legacy": {"value": legacy_value, "unit": legacy_unit,
                   "metric_type": benchlib.canonical_metric_type(legacy_type),
                   "state": legacy_state, "source": legacy_source,
                   "spread_known": legacy_spread is not None},
        "comparable": (benchlib.canonical_metric_type(mtype)
                       == benchlib.canonical_metric_type(legacy_type)
                       and benchlib.canonical_state(state)
                       == benchlib.canonical_state(legacy_state)),
        "verdict": verdict["verdict"], "pass": bool(verdict["pass"]),
        "ratio": verdict.get("ratio"), "noise": verdict.get("noise"),
        "reasons": verdict["reasons"],
        "contaminated_samples": contaminated,
    }
    st = (r["acos"].get("stats") or {})
    if st.get("spread") is not None and st["spread"] > benchlib.MAX_SPREAD:
        r["noisy"] = True
        msg = ("measured spread %.1f%% > 15%%: this row is noisy and cannot PASS"
               % st["spread_pct"])
        if msg not in r["reasons"]:
            r["reasons"].append(msg)
    else:
        r["noisy"] = False
    if extra:
        r.update(extra)
    return r


# ---------------------------------------------------------------- measurements
def main():
    started = utc()
    if KEEP_ARTIFACTS:
        print("bench-d2: ACOS_BENCH_KEEP_ARTIFACTS=1: per-rep frames/tree/logs are kept "
              "(disk use grows with every rep)", file=sys.stderr)
    prov = benchlib.provenance(
        ROOT, [ACOS, ACOS_ART], profile="release", engine="art",
        gate=benchlib.host_gate(ROOT, check_lock=False),
        extra={"method": "ADVISOR item 1 / BENCHMARKS.md section 11; one ART run per "
                         "run-exclusive.sh acquisition"})
    run = {"schema": "acos-bench-d2/1", "utc": started, "utc_finished": None,
           "profile": "release", "verdict_eligible": prov.get("verdict_eligible"),
           "smoke": bool(QUICK), "settings": {"samples": SAMPLES, "mem_reps": MEM_REPS,
                                              "cohorts": COHORTS, "suite_max": SUITE_MAX,
                                              "apk": os.path.relpath(APK, ROOT),
                                              "apk_sha256": benchlib.sha256_file(APK),
                                              "scenario": os.path.relpath(SCEN, ROOT),
                                              "scenario_sha256": benchlib.sha256_file(SCEN),
                                              "engine": "art"},
           "provenance": prov, "rows": [], "samples": {"latency": [], "cohorts": {},
                                                      "suite": []}}

    # ---- M3/M5/M7/M8 latency: one `acos verify --engine art` per acquisition --
    lat = []
    for i in range(SAMPLES):
        s = verify_sample("lat-%03d" % i)
        print("bench-d2: latency %d/%d ok=%s first_frame=%s ms host_load=%s"
              % (i + 1, SAMPLES, s.get("ok"), s.get("first_frame_ms"),
                 s["run"]["host_before"].get("loadavg1")), file=sys.stderr)
        lat.append(s)
        prune_artifacts(os.path.join(ARTI, s["tag"]), s["tag"])
    run["samples"]["latency"] = lat
    good = [s for s in lat if s.get("parse_ok") and s.get("ok") and not s["run"]["contaminated"]]
    contaminated = len([s for s in lat if s["run"]["contaminated"]])
    failures = [s["tag"] for s in lat if not s.get("ok")]

    def vals(key):
        return [s.get(key) for s in good if s.get(key) is not None]

    ff, capture, tree, tap0 = vals("first_frame_ms"), vals("capture_ms"), vals("tree_text_ms"), vals("tap0_ms")
    fst, cst, tst, tapst = stat_of(ff), stat_of(capture), stat_of(tree), stat_of(tap0)
    m3st = stat_of(vals("first_frame_ms_with_timer_overhead"))
    rows = []
    rows.append(row(
        "M1", "Fixed idle core memory (booted framework, no interaction, 60 s settle)",
        "MiB", "CGROUP_ANON_SHMEM", "warm", 206.0, "MiB", "CGROUP_ANON_SHMEM", "warm",
        decide(None, 206.0, mtype="CGROUP_ANON_SHMEM", state="warm", legacy_state="warm",
               extra_reasons=("NOT MEASURED: needs a held ART session that stays alive 60 s "
                              "after first frame to settle cgroup memory.stat anon+shmem; "
                              "`acos verify --engine art` exits after its assertion and this "
                              "build has no held-session ART path (BENCHMARKS.md 11.7). Not "
                              "estimated.",)),
        None, None, "no held ART session; 60 s settle unreachable on the verify path", 0,
        legacy_source="BENCHMARKS.md 3 (legacy ACVM W1 bare core, anon+shmem, settled 60 s)"))
    rows.append(row(
        "M2", "Per-session memory at N=5 (incremental and marginal)",
        "MiB/session", "CGROUP_MEMORY_CURRENT", "cold-process", 78.7, "MiB",
        "CGROUP_ANON_SHMEM", "warm",
        decide(None, 78.7, mtype="CGROUP_MEMORY_CURRENT", legacy_type="CGROUP_ANON_SHMEM",
               state="cold-process", legacy_state="warm",
               extra_reasons=("legacy column is cgroup anon+shmem settled 60 s (warm); this "
                              "run samples cgroup memory.current peak/at-exit, a different "
                              "type and state => NOT_COMPARABLE, never a win",)),
        None, None, "cohorts measured below; the legacy-comparable settled-60 s column is NOT_MEASURED",
        0, legacy_source="BENCHMARKS.md 3 (v9 per-session N=5, anon+shmem, warm, 60 s)"))
    rows.append(row(
        "M3", "Cold start -> first correct frame", "ms", "WALL_MS", "cold",
        3620.0, "ms", "WALL_MS", "cold",
        decide(m3st, 3620.0, mtype="WALL_MS", state="cold", legacy_state="cold",
               legacy_n=20, contaminated=contaminated,
               extra_reasons=("ACOS state is cold-process (no ACOS process alive, page cache "
                              "warm), the same cold category as legacy. First frame = cumulative "
                              "step ms through the first step with frame_seq>=1 plus the measured "
                              "engine-timer overhead (resources.wall_ms - sum(step durations)); "
                              "host exec/lock/guard startup is excluded. The scenario's count=0 "
                              "assertion binds correctness at that frame.",)),
        m3st["p50"] if m3st else None, m3st,
        "acos verify --engine art: engine timer start -> first fresh frame (cold process, one "
        "rep per lock acquisition); legacy = v12 cold core->first shot", m3st["n"],
        legacy_source="BENCHMARKS.md 3 (legacy v12 cold core->first shot 3.620 s)"))
    rows.append(row(
        "M4", "Worker create -> correct frame (warm core)", "ms", "WALL_MS", "warm",
        1297.0, "ms", "WALL_MS", "warm",
        decide(None, 1297.0, mtype="WALL_MS", state="warm", legacy_state="warm",
               extra_reasons=("NOT MEASURED: requires a pre-booted warm ART guest pool "
                              "(ADVISOR item 7); `acos verify --engine art` boots a fresh guest "
                              "per task (ArtSession::start), so no warm create exists to time. "
                              "Not estimated.",)),
        None, None, "no warm ART guest pool in this build", 0,
        legacy_source="BENCHMARKS.md 3 (legacy worker create->frame p50 1.251-1.343 s)"))
    rows.append(row(
        "M5", "Fresh screenshot retrieval", "ms", "WALL_MS", "cold-process",
        66.5, "ms", "WALL_MS", "warm",
        decide(cst, 66.5, mtype="WALL_MS", state="cold-process", legacy_state="warm",
               legacy_n=20, contaminated=contaminated,
               extra_reasons=("state/scope: capture step inside a cold-process verify session "
                              "and the CLI writes the PNG; legacy 66.5 ms is a warm-session "
                              "retrieval without a PNG write. Gate would need the no-write row, "
                              "which this CLI does not expose.",)),
        cst["p50"] if cst else None, cst,
        "`capture: fresh` step duration (includes PNG write) in each cold-process verify run",
        cst["n"], legacy_source="BENCHMARKS.md 3 (legacy fresh screenshot 0.066-0.067 s warm)"))
    rows.append(row(
        "M6", "Reset -> ready", "ms", "WALL_MS", "warm", 748.1, "ms", "WALL_MS", "warm",
        decide(None, 748.1, mtype="WALL_MS", state="warm", legacy_state="warm",
               extra_reasons=("NOT MEASURED: the ART launch engine has no session reset command "
                              "(scenarios/v2-art-counter.yaml: Reset is the app's own button); "
                              "tapping the app's Reset and observing state is not a reset->ready "
                              "metric. The app-reset tap->assert value is recorded in detail, "
                              "never substituted for M6.",)),
        None, None, "no ART session reset; measured app Reset tap->assert = %s ms"
        % (stat_of(vals("reset_app_tap_to_assert_ms")) or {}).get("p50"), 0,
        extra={"app_reset_tap_to_assert_ms": stat_of(vals("reset_app_tap_to_assert_ms"))},
        legacy_source="iter/D2.md (legacy reset --app -> frame 748.1 ms p50, warm)"))
    rows.append(row(
        "M7", "UI tree with text", "ms", "WALL_MS", "cold-process", 538.0, "ms",
        "WALL_MS", "warm",
        decide(tst, 538.0, mtype="WALL_MS", state="cold-process", legacy_state="warm",
               contaminated=contaminated,
               extra_reasons=("ACOS value is the first assert_text observation on the live tree "
                              "inside a cold-process verify session. Legacy one-shot text tree "
                              "538-768 ms is warm; the legacy persistent route is 4.1 ms. A "
                              "state mismatch is NOT_COMPARABLE; ACOS has no warm held-session "
                              "tree row yet.",)),
        tst["p50"] if tst else None, tst,
        "first assert_text (live tree read) per verify run", tst["n"],
        legacy_source="BENCHMARKS.md 3 (legacy one-shot text tree 538-768 ms warm)"))
    rows.append(row(
        "M8", "Action -> state latency", "ms", "WALL_MS", "cold-process", None, "ms",
        "WALL_MS", "warm",
        decide(tapst, None, mtype="WALL_MS", state="cold-process", legacy_state="warm",
               contaminated=contaminated,
               extra_reasons=("capability verified on ART (%d/%d runs pass all steps, tap "
                              "Increment 0->1 bound to a fresh frame), but the legacy ACVM never "
                              "published a scalar action->state value, so the D2 comparison is "
                              "NOT_MEASURED (BENCHMARKS.md 9.8 called it a capability pass, not "
                              "a ratio)." % (len(good), len(lat)),)),
        tapst["p50"] if tapst else None, tapst,
        "first `tap Increment` step duration (waits for the action's fresh frame)", tapst["n"],
        extra={"capability_verified": len(good) == len(lat) and len(lat) > 0,
               "pass_fraction": "%d/%d" % (len(good), len(lat))},
        legacy_source="not published (BENCHMARKS.md 3 has no action->state value)"))
    rows.append(None)  # M9 placed after the cohorts are measured
    rows.append(None)  # M10 placed after the suite is measured

    def m9_failure_note(p10, t10, p20, t20):
        """W39: the W33 sentence about clang bus errors / pthread EAGAIN was a
        cold-cohort compiler storm; the W37 build-once cache removed it. State
        what this run actually saw, and never blame the app for a session
        failure."""
        failed = (t10 - p10) + (t20 - p20)
        if failed == 0:
            return ("No session failed (N=10 %d/%d, N=20 %d/%d): the W37 build-once cache "
                    "removed the cold-cohort compiler storm, so the old clang bus error / "
                    "pthread EAGAIN explanation no longer applies." % (p10, t10, p20, t20))
        return ("%d of %d sessions failed; the structured error of each failure is in the "
                "run JSON, and a failure is never re-labelled as app behaviour." % (failed, t10 + t20))

    # ---- cohorts: M2/M9, one acquisition per rep ----
    coh = {}
    for n in COHORTS:
        reps = []
        for k in range(1, MEM_REPS + 1):
            h = host_now()
            if n >= 10:
                projected = h["mem_available_gib"] - 0.35 * n
                if h["mem_available_gib"] < 30.0 or projected < 12.0:
                    print("bench-d2: skipping cohort N=%d rep=%d: MemAvailable %.1f GiB, "
                          "projected after %.0f MiB/session < 12 GiB"
                          % (n, k, h["mem_available_gib"], 350), file=sys.stderr)
                    reps.append({"n": n, "skipped": "MemAvailable/ROAM projection",
                                 "host": h})
                    continue
            print("bench-d2: cohort N=%d rep=%d (MemAvailable %.1f GiB, load1 %.2f)"
                  % (n, k, h["mem_available_gib"], h["loadavg1"]), file=sys.stderr)
            reps.append(cohort_rep(n, k))
            prune_artifacts(os.path.join(OUT, "cohort-N%d-rep%d" % (n, k)),
                            "cohort-N%d-rep%d" % (n, k))
        coh[n] = reps
        run["samples"]["cohorts"][str(n)] = reps

    def cohort_val(n, key, per_session=False):
        out = []
        for r in coh.get(n, []):
            ss = [s for s in (r.get("sessions") or [])
                  if s.get("cgroup_samples", 0) >= 1]
            vv = [s.get(key) for s in ss if s.get(key) is not None]
            if not vv:
                continue
            out.append(sum(vv) / len(vv) if per_session else sum(vv))
        return out

    def cohort_passes(n):
        p = sum(r.get("passed", 0) for r in coh.get(n, []))
        t = sum(len(r.get("sessions") or []) for r in coh.get(n, []))
        return p, t

    def raw_vals(n, key):
        """Per-session values from sessions that actually reached a guest cgroup."""
        out = []
        for r in coh.get(n, []):
            for s in (r.get("sessions") or []):
                if s.get("cgroup_samples", 0) >= 1 and s.get(key) is not None:
                    out.append(s[key])
        return out

    n1_anon = cohort_val(1, "anon_shmem_last_mib", per_session=True)
    n5_anon = cohort_val(5, "anon_shmem_last_mib", per_session=True)
    n5_peak = raw_vals(5, "cgroup_peak_mib")
    n1_peak = raw_vals(1, "cgroup_peak_mib")
    m2st = stat_of(n5_anon)
    m2_marginal = None
    if n1_anon and n5_anon:
        m2_marginal = ((sum(n5_anon) * 5) - (sum(n1_anon) * 1)) / 4.0 / len(n5_anon)
    m2_verdict = decide(
        m2st, 78.7, mtype="CGROUP_ANON_SHMEM", state="cold-process",
        legacy_state="warm",
        contaminated=sum(1 for r in coh.get(5, []) if r.get("contaminated")),
        extra_reasons=("ACOS value is cgroup memory.stat anon+shmem at process exit, "
                       "NOT settled 60 s after first frame; legacy 78.7 MiB is warm/60 s. "
                       "Same type name, different state/settle => NOT_COMPARABLE "
                       "(benchlib.compare_memory rule).",))
    mc = benchlib.compare_memory(
        {"value_mib": m2st["p50"] if m2st else None, "metric_type": "CGROUP_ANON_SHMEM",
         "state": "cold-process", "settle_s": None, "n": m2st["n"] if m2st else 0},
        benchlib.legacy_memory_reference(78.7))
    m2_marginal_cg = None
    if n1_peak and n5_peak:
        m2_marginal_cg = (((sum(n5_peak) / len(n5_peak)) * 5
                           - (sum(n1_peak) / len(n1_peak))) / 4.0)
    rows[1] = row(
        "M2", "Per-session memory at N=5 (incremental and marginal)", "MiB/session",
        "CGROUP_ANON_SHMEM", "cold-process", 78.7, "MiB", "CGROUP_ANON_SHMEM", "warm",
        m2_verdict, m2st["p50"] if m2st else None, m2st,
        "Σ anon+shmem at exit / N=5 across %d reps (marginal (Σ5-Σ1)/4 = %s MiB; cgroup peak "
        "per session N=1 %s / N=5 %s MiB; peak marginal %s)"
        % (len(n5_anon), round(m2_marginal, 2) if m2_marginal is not None else "n/a",
           round(stat_of(n1_peak)["p50"], 2) if n1_peak else "n/a",
           round(stat_of(n5_peak)["p50"], 2) if n5_peak else "n/a",
           round(m2_marginal_cg, 2) if m2_marginal_cg is not None else "n/a"),
        m2st["n"], legacy_source="BENCHMARKS.md 3 (v9 per-session N=5, anon+shmem, warm, 60 s)",
        extra={"compare_memory": mc,
               "marginal_anon_shmem_mib": (round(m2_marginal, 2)
                                           if m2_marginal is not None else None),
               "marginal_cgroup_peak_mib": (round(m2_marginal_cg, 2)
                                            if m2_marginal_cg is not None else None),
               "per_session_cgroup_peak_n1_mib": stat_of(n1_peak) if n1_peak else None,
               "per_session_cgroup_peak_n5_mib": stat_of(n5_peak) if n5_peak else None,
               "cohort_shape": "one ART run per rep: N concurrent `acos verify` in one "
                               "run-exclusive acquisition; guest cgroup sampled every 100 ms"})

    n10_peak = raw_vals(10, "cgroup_peak_mib")
    n20_peak = raw_vals(20, "cgroup_peak_mib")
    p10, t10 = cohort_passes(10)
    p20, t20 = cohort_passes(20)
    n10st = stat_of(n10_peak)
    n20st = stat_of(n20_peak)
    m9st = n10st if n10st and n10st.get("p50") is not None else n20st
    rows[8] = row(
        "M9", "Concurrent workloads", "MiB/session (cgroup peak)", "CGROUP_MEMORY_CURRENT",
        "cold-process", None, "scorecard", "CGROUP_MEMORY_CURRENT", "warm",
        decide(m9st, None, mtype="CGROUP_MEMORY_CURRENT", state="cold-process",
               legacy_state="warm",
               contaminated=sum(1 for r in coh.get(10, []) + coh.get(20, [])
                                if r.get("contaminated")),
               extra_reasons=("NOT MEASURED as a D2 row: legacy M9 is a scorecard (N=5 plus 10 "
                              "virtual displays), not a scalar of this type; ACOS correctness "
                              "here is N=10 %d/%d and N=20 %d/%d passes, but independent state "
                              "with a distinct count k per session is not implemented, and the "
                              "legacy-comparable workset column does not exist. %s cgroup peak "
                              "per session is reported, never compared to legacy anon+shmem."
                              % (p10, t10, p20, t20, m9_failure_note(p10, t10, p20, t20)),)),
        m9st["p50"] if m9st else None, m9st,
        "N=10 %d/%d passes, cgroup peak %s MiB/session; N=20 %d/%d passes, cgroup peak %s "
        "MiB/session (one acquisition per rep; %d rep(s) per N)"
        % (p10, t10, round(n10st["p50"], 2) if n10st and n10st.get("p50") else "n/a",
           p20, t20, round(n20st["p50"], 2) if n20st and n20st.get("p50") else "n/a",
           MEM_REPS),
        m9st["n"] if m9st else 0,
        legacy_source="EXPERIMENTS.md E3.2/P1.5 (N=5 scorecard + 10 virtual displays; no scalar)",
        extra={"passes": {"N10": [p10, t10], "N20": [p20, t20]},
               "per_session_cgroup_peak_n10_mib": n10st,
               "per_session_cgroup_peak_n20_mib": n20st})

    # ---- suite: M10 (and M11 numerator) ----
    suite = []
    try:
        import yaml
        idx = yaml.safe_load(open(os.path.join(ROOT, "scenarios/suite/INDEX.yaml")))
        tasks = [t for t in idx.get("tasks", []) if not t.get("unsupported")][:SUITE_MAX]
    except Exception as exc:  # missing suite is NOT_MEASURED, not guessed
        tasks = []
        print("bench-d2: suite index unavailable: %s" % exc, file=sys.stderr)
    for t in tasks:
        scen = os.path.join(ROOT, "scenarios/suite", t["good"]["scenario"])
        apk = os.path.join(ROOT, t["apk"])
        adir = os.path.join(ARTI, "suite-%s" % t["id"])
        rec, out = art_run([ACOS, "verify", "--scenario", scen, "--apk", apk,
                            "--engine", "art", "--artifacts", adir, "--json"],
                           "suite-%s" % t["id"])
        passed, wall_ms, status = False, None, None
        try:
            d = json.loads(out)
            v = (d.get("result") or {}).get("verdict") or {}
            status = v.get("status") or d.get("status")
            passed = d.get("ok") is True and status == "pass"
            wall_ms = (v.get("resources") or {}).get("wall_ms")
        except ValueError:
            pass
        suite.append({"id": t["id"], "scenario": t["good"]["scenario"],
                      "pass": passed, "status": status, "wall_ms": wall_ms,
                      "run": rec})
        print("bench-d2: suite %s pass=%s wall=%s ms load1=%s"
              % (t["id"], passed, wall_ms, rec["host_before"].get("loadavg1")),
              file=sys.stderr)
        prune_artifacts(os.path.join(ARTI, "suite-%s" % t["id"]), "suite-%s" % t["id"])
    run["samples"]["suite"] = suite
    walls = [s["wall_ms"] for s in suite if s.get("wall_ms") is not None]
    sst = stat_of(walls)
    verified = sum(1 for s in suite if s["pass"])
    rows[9] = row(
        "M10", "End-to-end verify wall", "ms", "WALL_MS", "cold-process", 19000.0, "ms",
        "WALL_MS", "warm",
        decide(sst, 19000.0, mtype="WALL_MS", state="cold-process", legacy_state="warm",
               legacy_n=20,
               contaminated=sum(1 for s in suite if s["run"]["contaminated"]),
               extra_reasons=("NOT COMPARABLE on scope/state: ACOS value is `acos verify "
                              "--engine art` on prebuilt fixture APKs (no build/install stage) "
                              "inside a cold process; legacy ~19 s is the U2 dev loop build->"
                              "install->launch->assert->evidence->reset->cleanup (install-bound, "
                              "warm). Suite: %d/%d good tasks pass; a wall on a failing task is "
                              "still a cost sample, never a pass." % (verified, len(suite)),)),
        sst["p50"] if sst else None, sst,
        "p50 wall per T01-T12 good task (%d expressible tasks of 12; the rest name gaps in "
        "INDEX.yaml `unsupported` and are not run), 1 run each, one acquisition per task"
        % len(suite), sst["n"],
        extra={"verified_good": verified, "tasks_run": len(suite),
               "suite": [{"id": s["id"], "pass": s["pass"], "wall_ms": s["wall_ms"]}
                         for s in suite]},
        legacy_source="docs/handoff/04 + BENCHMARKS.md 3 (legacy U2 loop ~19 s install-bound)")
    # M11: formula implemented; only filled from an N=10 suite with job integrals.
    m11_value = None
    m11_reason = ("NOT MEASURED: M11 = verified_good / Σ job_gib_min over the T01-T12 suite at "
                  "N=10 concurrent, 3 rounds, with per-job cgroup memory.current integrals. This "
                  "run's suite is N=1 (one job per acquisition) and records wall only, and the "
                  "N=10 cohorts here run the counter scenario, not the suite; no denominator of "
                  "the defined kind exists yet (W30 owns the N=10 suite runner). Legacy never "
                  "published M11. Formula is implemented in benchlib.sample_integral; nothing is "
                  "estimated.")
    rows.append(row(
        "M11", "Verified tasks / GiB-minute (north star)", "tasks/GiB-min", "RATIO",
        "cold-process", None, "tasks/GiB-min", "RATIO", "warm",
        decide(None, None, mtype="RATIO", state="cold-process", legacy_state="warm",
               extra_reasons=(m11_reason,)),
        m11_value, None, m11_reason, 0,
        extra={"formula": "verified_good / Σ job_gib_min; job_gib_min = "
                          "Σ memory.current_bytes/2^30 × dt_s/60 per job cgroup; "
                          "if false_pass > 0 then M11 := 0",
               "legacy_derived_bar": "((206/10)+112.1)/1024*(19/60)=0.0410 GiB*min => 24.4 "
                                     "tasks/GiB*min (legacy-derived bound; not a published value)",
               "suite_verified_good_n1": verified, "suite_tasks_run": len(suite)},
        legacy_source="not published (BENCHMARKS.md 1/2; legacy-derived bound 24.4 only)"))

    run["rows"] = rows
    run["utc_finished"] = utc()
    run["host_gate_end"] = benchlib.host_gate(ROOT, check_lock=False)
    summary = {"counts": {}, "false_pass": 0, "all_rows_have_reason": True,
               "latency_failures": failures, "latency_contaminated": contaminated,
               "cohort_passes": {str(n): cohort_passes(n) for n in COHORTS},
               "suite_verified_good": verified, "suite_tasks_run": len(suite)}
    for r in rows:
        summary["counts"][r["verdict"]] = summary["counts"].get(r["verdict"], 0) + 1
        if not r["reasons"]:
            summary["all_rows_have_reason"] = False
    run["summary"] = summary
    errors = validate(run)
    run["validation_errors"] = errors
    if not errors:
        benchlib.write_json(RESULT_JSON, run)
    else:
        benchlib.write_json(RESULT_JSON.replace(".json", ".invalid.json"), run)
    with open(os.path.join(OUT, "summary.json"), "w") as f:
        json.dump(run, f, indent=2, sort_keys=True)
    table = render(run)
    with open(RESULT_MD, "w") as f:
        f.write(table)
    print(table)
    print("bench-d2: json %s%s" % (RESULT_JSON, "" if not errors else " (INVALID)"))
    print("bench-d2: validation %s" % ("ok" if not errors else errors))
    return 0 if not errors else 1


def validate(run):
    errs = []
    ids = [r["id"] for r in run.get("rows", [])]
    want = ["M%d" % i for i in range(1, 12)]
    if sorted(ids) != sorted(want):
        errs.append("rows must be M1..M11 (got %s)" % ids)
    prov = run.get("provenance") or {}
    for key in ("utc", "git_head", "profile", "verdict_eligible", "binaries"):
        if key not in prov:
            errs.append("provenance.%s missing" % key)
    if prov.get("profile") != "release" or prov.get("verdict_eligible") is not True:
        errs.append("provenance is not release/verdict_eligible")
    for r in run.get("rows", []):
        for key in ("id", "metric", "unit", "acos", "legacy", "comparable",
                    "verdict", "reasons", "contaminated_samples"):
            if key not in r:
                errs.append("%s.%s missing" % (r.get("id"), key))
        if r.get("verdict") not in ALLOWED:
            errs.append("%s verdict %r not allowed" % (r.get("id"), r.get("verdict")))
        if not isinstance(r.get("reasons"), list) or not r["reasons"]:
            errs.append("%s has no verdict reason" % r.get("id"))
        a = r.get("acos") or {}
        if a.get("value") is not None and a.get("stats") is None:
            errs.append("%s has a value but no stats" % r.get("id"))
        st = a.get("stats")
        if st:
            for key in ("n", "p50", "p5", "p95", "spread", "ci95_low", "ci95_high"):
                if key not in st:
                    errs.append("%s.stats.%s missing" % (r.get("id"), key))
    if run.get("summary", {}).get("all_rows_have_reason") is not True:
        errs.append("a row has no reason")
    return errs


def fmt(v, d=2):
    if v is None:
        return "n/a"
    if isinstance(v, float):
        return "%.*f" % (d, v)
    return str(v)


def render(run):
    out = []
    out.append("# D2 on ART (release) -- %s" % run["utc"])
    out.append("")
    if run.get("smoke"):
        out.append("**SMOKE RUN (`--quick`): rep counts are below the advisor's gate, so these "
                   "verdicts are pipeline evidence only, never a D2 result.**")
        out.append("")
    out.append("Engine `art`, profile `%s`, one ART run per `run-exclusive.sh` acquisition "
               "(lock never held across reps). %d latency samples, %d memory reps per cohort."
               % (run["profile"], run["settings"]["samples"], run["settings"]["mem_reps"]))
    out.append("")
    out.append("| # | Metric | ACOS-ART value | n | 95% CI (p50) | spread | state / type "
               "| Legacy ACVM | Legacy type / state | Same type+state | Verdict | Reason |")
    out.append("|---|---|---|---|---|---|---|---|---|---|---|---|")
    for r in run["rows"]:
        a = r["acos"]; st = a.get("stats") or {}
        val = a.get("value")
        if val is None:
            val_s = "not measured"
        elif r["unit"] in ("ms",):
            val_s = "%.1f ms" % float(val)
        else:
            val_s = "%s %s" % (fmt(val), r["unit"])
        ci = ("%.2f..%.2f" % (st["ci95_low"], st["ci95_high"])
              if st.get("ci95_low") is not None else "n/a")
        spread = ("%.1f%%" % st["spread_pct"] if st.get("spread_pct") is not None
                  else "n/a")
        leg = r["legacy"]
        leg_s = ("%s %s" % (fmt(leg["value"]), leg["unit"])
                 if leg.get("value") is not None else "not published")
        comp = ("yes" if r["comparable"] else "no")
        out.append("| %s | %s | %s | %s | %s | %s | %s / %s | %s | %s / %s | %s | **%s** | %s |"
                   % (r["id"], r["metric"], val_s, st.get("n", a.get("n", 0)), ci, spread,
                      a.get("state"), a.get("metric_type"), leg_s,
                      leg.get("metric_type"), leg.get("state"), comp, r["verdict"],
                      " ".join(r["reasons"])))
    out.append("")
    s = run.get("summary", {})
    out.append("Verdict counts: " + ", ".join("%s=%d" % (k, v)
                                              for k, v in sorted(s.get("counts", {}).items())))
    out.append("")
    out.append("False passes: %d. No PASS is possible without same metric type+state, "
               "non-noisy spread and a beat larger than noise; a contaminated sample can never "
               "PASS. A TIE is not a pass." % s.get("false_pass", 0))
    out.append("")
    out.append("## Samples and host load (per lock acquisition)")
    out.append("")
    out.append("| sample | ok | first frame ms | capture ms | tree ms | tap ms | wall s "
               "| load1 before/after | MemAvailable before GiB | contaminated |")
    out.append("|---|---|---|---|---|---|---|---|---|---|")
    for x in run["samples"].get("latency", []):
        hb, ha = x["run"]["host_before"], x["run"]["host_after"]
        out.append("| %s | %s | %s | %s | %s | %s | %s | %s / %s | %s | %s |"
                   % (x["tag"], x.get("ok"), fmt(x.get("first_frame_ms"), 1),
                      fmt(x.get("capture_ms"), 1), fmt(x.get("tree_text_ms"), 1),
                      fmt(x.get("tap0_ms"), 1), fmt(x["run"]["wall_s"], 2),
                      hb.get("loadavg1"), ha.get("loadavg1"),
                      fmt(hb.get("mem_available_gib"), 2),
                      x["run"]["contaminated"]))
    out.append("")
    out.append("## Cohorts (one acquisition per rep; guest cgroup sampled every 100 ms)")
    out.append("")
    out.append("| N | rep | passes | per-session cgroup peak MiB (p50) | per-session "
               "anon+shmem at exit MiB (p50) | Σ GiB-min | contaminated |")
    out.append("|---|---|---|---|---|---|---|")
    for n, reps in run["samples"].get("cohorts", {}).items():
        for k, rr in enumerate(reps, 1):
            ss = rr.get("sessions") or []
            peaks = [x["cgroup_peak_mib"] for x in ss if x.get("cgroup_peak_mib")]
            ans = [x["anon_shmem_last_mib"] for x in ss if x.get("anon_shmem_last_mib") is not None]
            gib = sum(x.get("gib_min") or 0.0 for x in ss)
            ps = stat_of(peaks) if peaks else None
            as_ = stat_of(ans) if ans else None
            out.append("| %s | %d | %s/%d | %s | %s | %.4f | %s |"
                       % (n, k, rr.get("passed"), len(ss),
                          fmt(ps["p50"]) if ps else "n/a",
                          fmt(as_["p50"]) if as_ else "n/a", gib,
                          rr.get("contaminated")))
    out.append("")
    out.append("## Suite (M10 cost samples; one acquisition per task)")
    out.append("")
    out.append("| task | pass | wall ms | load1 | contaminated |")
    out.append("|---|---|---|---|---|")
    for x in run["samples"].get("suite", []):
        out.append("| %s | %s | %s | %s | %s |"
                   % (x["id"], x["pass"], fmt(x["wall_ms"], 1),
                      x["run"]["host_before"].get("loadavg1"), x["run"]["contaminated"]))
    out.append("")
    out.append("## Provenance")
    out.append("")
    out.append("```yaml")
    out.append("utc: %s" % run["utc"])
    out.append("git_head: %s" % run["provenance"].get("git_head"))
    out.append("git_dirty: %s (%s files, porcelain sha256 %s)"
               % (run["provenance"].get("git_dirty"),
                  run["provenance"].get("git_dirty_files"),
                  (run["provenance"].get("git_status_porcelain_sha256") or "")[:16]))
    out.append("profile: %s, verdict_eligible: %s"
               % (run["provenance"].get("profile"), run["provenance"].get("verdict_eligible")))
    for b in run["provenance"].get("binaries", []):
        out.append("binary: %s sha256=%s" % (b["path"], b["sha256"]))
    out.append("apk_sha256: %s" % run["settings"].get("apk_sha256"))
    out.append("scenario_sha256: %s" % run["settings"].get("scenario_sha256"))
    out.append("rustc: %s" % run["provenance"].get("rustc"))
    out.append("```")
    out.append("")
    return "\n".join(out) + "\n"


if __name__ == "__main__":
    if len(sys.argv) == 3 and sys.argv[1] == "--analyze":
        d = sys.argv[2]
        run = json.load(open(os.path.join(d, "summary.json")))
        run["utc_analyzed"] = utc()
        errs = validate(run)
        table = render(run)
        with open(os.path.join(d, "table.md"), "w") as f:
            f.write(table)
        print(table)
        print("validation: %s" % ("ok" if not errs else errs))
        sys.exit(0 if not errs else 1)
    sys.exit(main())
PY

exec python3 -I "$OUT/driver.py" "$ROOT" "$OUT" "$ACOS" "$ACOS_ART" \
    "$SAMPLES" "$MEM_REPS" "$COHORTS" "$SUITE_MAX" "$QUICK"
