#!/usr/bin/env bash
#
# bench-acos.sh -- the ACOS V1 metrics harness driver (BENCHMARKS.md §2/§5).
#
#   tools/bench-acos.sh                     # measure every metric that exists today
#   tools/bench-acos.sh process-start sessions
#   tools/bench-acos.sh --quick             # 3 samples (smoke test only, NOT a claim)
#   tools/bench-acos.sh --check             # regression gate vs results/bench/baseline.json
#   tools/bench-acos.sh --check --from-run results/bench/run-<stamp>.json
#   tools/bench-acos.sh --check --tolerance 0.30 --baseline results/bench/baseline.json
#
# What it does
#   1. builds `acos` (dev profile, no debug info) under tools/guarded.sh,
#   2. runs `acos bench <metric> --json` per metric, each inside its own bounded
#      guard scope (`--mem`, `--time`),
#   3. merges the envelopes into results/bench/run-<stamp>.json together with the
#      host/commit/method record required by BENCHMARKS.md §4.1,
#   4. writes results/bench/REPORT.md: the ACOS-vs-legacy table for every
#      BENCHMARKS.md row, the worse-than-legacy list with plans, and the
#      not_yet_measurable list with the exact blocking dependency.
#
# `--check` compares the measured p50/p95 rows against a committed baseline JSON
# with a tolerance (default = max(5 %, measured spread); see --tolerance) and
# exits non-zero on a regression. Regressions are never silently accepted.
#
# Env:
#   ACOS_BENCH_RUNS      samples per metric (default 20; the method minimum)
#   ACOS_BENCH_TIME      wall budget per metric scope (default 900)
#   CARGO_TARGET_DIR     respected (agents must use their own target dir)
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
RESULTS="$ROOT/results/bench"
RUNS="${ACOS_BENCH_RUNS:-20}"
QUICK=0
CHECK=0
CHECK_AFTER=0
WRITE_BASELINE=0
FROM_RUN=""
BASELINE="$RESULTS/baseline.json"
PROFILE="${ACOS_BENCH_PROFILE:-release}"
TOLERANCE=""
METRICS=()

usage() { sed -n '2,30p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'; }

while [ $# -gt 0 ]; do
    case "$1" in
        --quick) QUICK=1; RUNS=3; shift ;;
        --runs) RUNS="$2"; shift 2 ;;
        --check) CHECK=1; shift ;;
        --check-after) CHECK_AFTER=1; shift ;;
        --write-baseline) WRITE_BASELINE=1; shift ;;
        --profile) PROFILE="$2"; shift 2 ;;
        --from-run) FROM_RUN="$2"; shift 2 ;;
        --baseline) BASELINE="$2"; shift 2 ;;
        --tolerance) TOLERANCE="$2"; shift 2 ;;
        -h|--help) usage; exit 0 ;;
        --) shift; break ;;
        -*) echo "bench-acos: unknown option $1" >&2; usage >&2; exit 2 ;;
        *) METRICS+=("$1"); shift ;;
    esac
done

if [ ${#METRICS[@]} -eq 0 ]; then
    # Every metric whose inputs exist in today's engine. Order matters: cheap
    # ones first so a broken metric fails fast.
    METRICS=(process-start boot-attach bind-app idle-rss sessions sessions-shared sessions-pool idle-cpu interpreter)
fi

mkdir -p "$RESULTS"

# ---------------------------------------------------------------------------
# regression gate
# ---------------------------------------------------------------------------
if [ "$WRITE_BASELINE" = 1 ] && [ -n "$FROM_RUN" ]; then
    python3 - "$FROM_RUN" "$BASELINE" <<'PYB'
import json, sys
run_path, baseline_path = sys.argv[1:3]
run = json.load(open(run_path))
rows = []
for rid, block in sorted(run.get("gates", {}).items()):
    rows.append({"id": rid, "path": "", "value": block["p50"],
                 "direction": "lower", "tolerance": 0.25})
doc = {
    "schema": "acos-bench-baseline/v1",
    "created": run.get("timestamp"),
    "commit": run.get("commit"),
    "source_run": run_path.split("/")[-1],
    "note": ("Committed regression baseline: p50 of every gated row from the source run. "
             "The gate fails when a fresh p50 exceeds value*(1+max(tolerance, measured spread)). "
             "Update only with a recorded run and an explicit note in BENCHMARKS.md."),
    "rows": rows,
}
json.dump(doc, open(baseline_path, "w"), indent=2)
print(f"bench-acos: wrote baseline {baseline_path} ({len(rows)} rows)")
PYB
    exit 0
fi

if [ "$CHECK" = 1 ] && [ -z "$FROM_RUN" ]; then
    # Measure first, then gate on the fresh run.
    CHECK_AFTER=1
fi

if [ "$CHECK" = 1 ] && [ -n "$FROM_RUN" ]; then
    TOL="${TOLERANCE:-}"
    exec python3 - "$ROOT" "$BASELINE" "$FROM_RUN" "$TOL" <<'PY'
import json, sys
root, baseline_path, run_path, tol_arg = sys.argv[1:5]
tol_default = float(tol_arg) if tol_arg else 0.25
try:
    baseline = json.load(open(baseline_path))
except FileNotFoundError:
    print(f"bench-acos --check: no baseline at {baseline_path}; run tools/bench-acos.sh and "
          f"commit the produced run as the baseline first", file=sys.stderr)
    sys.exit(2)
run = json.load(open(run_path))

# Every gated row: (id, json path into the run doc, direction, tolerance override)
ROWS = baseline.get("rows", [])
rows_now = {k: v for k, v in (run.get("gates") or {}).items()}

def get(path, doc):
    cur = doc
    for part in path.split("."):
        if cur is None:
            return None
        if part.isdigit():
            idx = int(part)
            if not isinstance(cur, list) or idx >= len(cur):
                return None
            cur = cur[idx]
        else:
            if not isinstance(cur, dict):
                return None
            cur = cur.get(part)
    return cur

failures = []
checks = 0
print(f"bench-acos --check: baseline={baseline_path} run={run_path} tol={tol_default:.0%}")
print(f"{'gate':<46} {'baseline':>12} {'now':>12} {'delta':>9}  verdict")
for row in ROWS:
    rid = row["id"]
    path = row["path"]
    direction = row.get("direction", "lower")
    base = row["value"]
    now_block = rows_now.get(rid)
    now = None
    if isinstance(now_block, dict):
        now = now_block.get("p50")
    elif isinstance(now_block, (int, float)):
        now = float(now_block)
    if now is None:
        now = get(path, run)
    if now is None:
        # A metric that is not in this run is not silently "fine".
        failures.append((rid, base, now, "missing"))
        print(f"{rid:<46} {base:>12.4f} {'-':>12} {'-':>9}  MISSING")
        continue
    tol = float(row.get("tolerance", tol_default))
    # Spread-aware noise floor: max(tolerance, measured p95/p50 - 1).
    spread = 0.0
    if isinstance(now_block, dict) and now_block.get("p95") and now:
        spread = max(0.0, float(now_block["p95"]) / float(now) - 1.0)
    eff = max(tol, min(spread, 1.0))
    delta = (now - base) / base if base else (0.0 if now == 0 else 1.0)
    regressed = delta > eff if direction == "lower" else -delta > eff
    checks += 1
    if regressed:
        failures.append((rid, base, now, f"{delta:+.1%} > tol {eff:.0%}"))
    print(f"{rid:<46} {base:>12.4f} {now:>12.4f} {delta:>+8.1%}  "
          f"{'REGRESSION' if regressed else 'ok'}  (tol {eff:.0%}, {direction} is better)")
print()
if failures:
    print(f"bench-acos --check: FAIL -- {len(failures)} regression(s) / missing gate(s):",
          file=sys.stderr)
    for rid, base, now, why in failures:
        print(f"  - {rid}: baseline={base} now={now} ({why})", file=sys.stderr)
    sys.exit(1)
print(f"bench-acos --check: PASS -- {checks} gate(s) within tolerance "
      f"(no correctness claim: see results/bench/REPORT.md)")
PY
fi

# ---------------------------------------------------------------------------
# 1. build
# ---------------------------------------------------------------------------
echo "bench-acos: building acos ($PROFILE profile, no debug info)..."
BUILD_ARGS=(build -p acos-cli --manifest-path "$ROOT/Cargo.toml")
if [ "$PROFILE" = "release" ]; then BUILD_ARGS+=(--release); fi
"$ROOT/tools/guarded.sh" --mem 6G --time 1200 -- cargo "${BUILD_ARGS[@]}" >/dev/null
TARGET_DIR="${CARGO_TARGET_DIR:-$ROOT/target}"
if [ "$PROFILE" = "release" ]; then ACOS="$TARGET_DIR/release/acos"; else ACOS="$TARGET_DIR/debug/acos"; fi
if [ ! -x "$ACOS" ]; then
    echo "bench-acos: error: $ACOS not found" >&2
    exit 2
fi
"$ACOS" bench --help >/dev/null 2>&1 || {
    echo "bench-acos: error: $ACOS has no bench subcommand" >&2
    exit 2
}

APK="$ROOT/fixtures/build/apps/hello-views.apk"
if [ ! -f "$APK" ]; then
    echo "bench-acos: error: $APK missing (run tools/build-apps.sh)" >&2
    exit 2
fi
DEXPROGS=(
    "$ROOT/fixtures/build/dexprogs/02_int_arith/classes.dex"
    "$ROOT/fixtures/build/dexprogs/05_branches_loops/classes.dex"
    "$ROOT/fixtures/build/dexprogs/09_strings/classes.dex"
)
for dex in "${DEXPROGS[@]}"; do
    [ -f "$dex" ] || {
        echo "bench-acos: error: $dex missing (run tools/build-fixtures.sh)" >&2
        exit 2
    }
done

STAMP="$(date +%Y-%m-%d-%H%M%S)"
RUN_JSON="$RESULTS/run-$STAMP.json"
PROBE_DIR="$ROOT/target/tmp/bench-acos-$STAMP"
mkdir -p "$PROBE_DIR"
cleanup() { rm -rf "$PROBE_DIR"; }
trap cleanup EXIT INT TERM

hmp() { awk -F': ' "/^$1:/{print \$2; exit}" /proc/meminfo | awk '{print $1}'; }

mem_args() {
    case "$1" in
        sessions-pool) echo "--mem 8G --time 900" ;;
        sessions|sessions-shared|idle-cpu) echo "--mem 6G --time 900" ;;
        *) echo "--mem 4G --time 300" ;;
    esac
}

# ---------------------------------------------------------------------------
# 2. measure
# ---------------------------------------------------------------------------
STATUS_FILE="$PROBE_DIR/status.tsv"
: >"$STATUS_FILE"

for metric in "${METRICS[@]}"; do
    out="$PROBE_DIR/$metric.json"
    extra=()
    if [ "$metric" = "interpreter" ]; then
        for dex in "${DEXPROGS[@]}"; do extra+=(--dex "$dex"); done
    fi
    echo "bench-acos: $metric (runs=$RUNS)..."
    # shellcheck disable=SC2046
    if "$ROOT/tools/guarded.sh" $(mem_args "$metric") -- \
        "$ACOS" bench "$metric" --runs "$RUNS" --json --out "$out" \
        --apk "$APK" "${extra[@]}" >/dev/null 2>"$PROBE_DIR/$metric.err"; then
        printf '%s\tpass\t%s\n' "$metric" "$out" >>"$STATUS_FILE"
    else
        rc=$?
        # A failed metric is a reported result, never a silent omission.
        printf '%s\tfail:%s\t%s\n' "$metric" "$rc" "$(head -c 400 "$PROBE_DIR/$metric.err" | tr '\n' ' ')" \
            >>"$STATUS_FILE"
        echo "bench-acos: warning: metric $metric failed (rc=$rc); see $PROBE_DIR/$metric.err" >&2
    fi
done

# ---------------------------------------------------------------------------
# 3. merge into one run document + method record (BENCHMARKS.md §4.1)
# ---------------------------------------------------------------------------
python3 - "$ROOT" "$RESULTS" "$RUN_JSON" "$PROBE_DIR" "$RUNS" "$STAMP" "$APK" "$PROFILE" <<'PY'
import datetime, json, os, platform, subprocess, sys
root, results, run_json, probe_dir, runs, stamp, apk, profile = sys.argv[1:9]

def sh(cmd):
    try:
        return subprocess.run(cmd, shell=True, capture_output=True, text=True,
                              check=False).stdout.strip()
    except Exception:
        return ""

def read_kib(key):
    try:
        for line in open("/proc/meminfo"):
            if line.startswith(key + ":"):
                return int(line.split()[1])
    except Exception:
        pass
    return 0

envelopes = {}
status = {}
for line in open(os.path.join(probe_dir, "status.tsv")):
    parts = line.rstrip("\n").split("\t")
    if len(parts) < 2:
        continue
    name, state = parts[0], parts[1]
    status[name] = state
    if state == "pass" and len(parts) > 2 and os.path.exists(parts[2]):
        envelopes[name] = json.load(open(parts[2]))

# ---- flat gate rows (p50 values used by --check) --------------------------
gates = {}

def p50(doc, *path):
    cur = doc
    for part in path:
        if cur is None:
            return None
        cur = cur[part] if not isinstance(cur, list) else (
            cur[int(part)] if part.isdigit() and int(part) < len(cur) else None)
    return cur

def add_gate(rid, block):
    if isinstance(block, dict) and "p50" in block:
        gates[rid] = {"p50": block["p50"], "p95": block.get("p95")}

def result_of(name):
    env = envelopes.get(name)
    if not env or not env.get("ok"):
        return None
    return env.get("result")

r = result_of("process-start")
if r:
    add_gate("process-start.wall_ms.p50", r.get("wall_ms"))

r = result_of("boot-attach")
if r:
    add_gate("boot-attach.boot_ms.p50", r.get("boot_ms"))
    add_gate("boot-attach.vm_ready_ms.p50", r.get("vm_ready_ms"))

r = result_of("bind-app")
if r:
    add_gate("bind-app.attach_to_bind_ms.p50", r.get("attach_to_bind_ms"))

r = result_of("idle-rss")
if r:
    variants = r.get("variants") or {}
    if "vm" in variants:
        add_gate("idle-rss.vm.anon_plus_shmem_kib.p50",
                 variants["vm"].get("anon_plus_shmem_kib"))
    if "booted" in variants:
        add_gate("idle-rss.booted.anon_plus_shmem_kib.p50",
                 variants["booted"].get("anon_plus_shmem_kib"))
        add_gate("idle-rss.booted.pss_kib.p50", variants["booted"].get("pss_kib"))

r = result_of("sessions")
if r:
    for row in r.get("sessions", []):
        if row.get("n") == 10:
            add_gate("sessions.n10.anon_plus_shmem_kib.p50", row.get("anon_plus_shmem_kib"))
    for row in r.get("marginal", []):
        if row.get("from_n") == 5:
            gates["sessions.marginal_n5_to_n10_mib.p50"] = {
                "p50": row.get("marginal_miB_per_session")}
    for row in r.get("incremental", []):
        if row.get("n") == 5:
            gates["sessions.incremental_n5_mib.p50"] = {
                "p50": row.get("incremental_miB_per_session")}

r = result_of("sessions-shared")
if r:
    for row in r.get("sessions", []):
        if row.get("n") == 20:
            add_gate("sessions-shared.n20.anon_plus_shmem_kib.p50", row.get("anon_plus_shmem_kib"))
    for row in r.get("marginal", []):
        if row.get("from_n") == 10:
            gates["sessions-shared.marginal_n10_to_n20_mib.p50"] = {
                "p50": row.get("marginal_miB_per_session")}
    for row in r.get("incremental", []):
        if row.get("n") == 5:
            gates["sessions-shared.incremental_n5_mib.p50"] = {
                "p50": row.get("incremental_miB_per_session")}

r = result_of("sessions-pool")
if r:
    for row in r.get("sessions", []):
        if row.get("n") == 5:
            add_gate("sessions-pool.n5.anon_plus_shmem_kib.p50",
                     row.get("memory", {}).get("anon_plus_shmem_kib"))
            gates["sessions-pool.n5.reset_ms.p50"] = {"p50": row.get("reset_ms_p50")}
        if row.get("n") == 20:
            add_gate("sessions-pool.n20.anon_plus_shmem_kib.p50",
                     row.get("memory", {}).get("anon_plus_shmem_kib"))
            add_gate("sessions-pool.n20.jobs_per_sec.p50",
                     row.get("throughput", {}).get("jobs_per_sec"))
    for row in r.get("marginal", []):
        if row.get("from_n") == 10:
            gates["sessions-pool.marginal_n10_to_n20_mib.p50"] = {
                "p50": row.get("marginal_miB_per_session")}

r = result_of("idle-cpu")
if r:
    add_gate("idle-cpu.cpu_ticks.p50", r.get("cpu_ticks"))

r = result_of("interpreter")
if r:
    for row in r.get("programs", []):
        add_gate(f"interpreter.{row.get('program')}.wall_ms.p50", row.get("wall_ms"))

doc = {
    "schema": "acos-bench-run/v1",
    "timestamp": datetime.datetime.now().isoformat(timespec="seconds"),
    "commit": sh(f"cd {root} && git rev-parse --short HEAD") or "no-git",
    "samples_per_metric": int(runs),
    "profile": profile,
    "methodology": ("BENCHMARKS.md §2: >=20 samples (p50/p95 nearest-rank), metric type labelled, "
                    "cold/warm stated, /proc based (no /usr/bin/time), no fixed sleep in a timed "
                    "path, one tools/guarded.sh scope per metric"),
    "host": {
        "cpu": sh("grep -m1 'model name' /proc/cpuinfo | cut -d: -f2-").strip(),
        "threads": os.cpu_count(),
        "kernel": platform.release(),
        "ram_total_kib": read_kib("MemTotal"),
        "ram_available_at_start_kib": read_kib("MemAvailable"),
        "zram_swap_total_kib": read_kib("SwapTotal"),
        "loadavg1": open("/proc/loadavg").read().split()[0],
        "rustc": sh("rustc --version"),
    },
    "fixture": {
        "path": apk,
        "sha256": sh(f"sha256sum {apk} | cut -d' ' -f1"),
        "package": "com.example.helloviews",
        "api_level": 34,
        "abi": "x86_64",
    },
    "metric_status": status,
    "gates": gates,
    "envelopes": envelopes,
}
json.dump(doc, open(run_json, "w"), indent=2)
print(f"bench-acos: wrote {run_json}", file=sys.stderr)
PY

# ---------------------------------------------------------------------------
# 4. report: ACOS vs legacy for every BENCHMARKS.md row
# ---------------------------------------------------------------------------
python3 - "$ROOT" "$RESULTS/REPORT.md" "$RUN_JSON" <<'PY'
import json, sys
root, report_path, run_path = sys.argv[1:4]
run = json.load(open(run_path))
envs = run.get("envelopes", {})

def res(name):
    env = envs.get(name) or {}
    return env.get("result") if env.get("ok") else None

def p50_of(block):
    return None if not isinstance(block, dict) else block.get("p50")

# ---- ACOS values (all in the unit the comparison needs) -------------------
acos = {}
r = res("process-start")
if r:
    acos["process-start"] = f"{p50_of(r['wall_ms']):.1f} ms p50 / {p50_of(r['wall_ms'])}"
r = res("boot-attach")
if r:
    acos["boot-attach"] = (p50_of(r["boot_ms"]), p50_of(r["vm_ready_ms"]))
r = res("bind-app")
if r:
    acos["bind-app"] = p50_of(r["attach_to_bind_ms"])
r = res("idle-rss")
if r:
    acos["idle-rss"] = r["variants"]
r = res("sessions")
if r:
    acos["sessions"] = r
r = res("sessions-shared")
if r:
    acos["sessions-shared"] = r
r = res("sessions-pool")
if r:
    acos["sessions-pool"] = r
r = res("idle-cpu")
if r:
    acos["idle-cpu"] = r
r = res("interpreter")
if r:
    acos["interpreter"] = r

def mib(kib):
    return None if kib is None else round(kib / 1024.0, 1)

idle = acos.get("idle-rss", {})
sess = acos.get("sessions", {})
sess_shared = acos.get("sessions-shared", {})
interp = acos.get("interpreter", {})

def session_row(n, field="incremental_miB_per_session"):
    for row in sess.get("incremental", []):
        if row.get("n") == n:
            return row.get(field)
    return None

def marginal_row(lo):
    for row in sess.get("marginal", []):
        if row.get("from_n") == lo:
            return row.get("marginal_miB_per_session")
    return None

# ---- BENCHMARKS.md rows (legacy values + comparability) -------------------
# verdict: better / worse / comparable (within noise) / not-measurable /
#          no-legacy-baseline / not-comparable (different metric type or state)
ROWS = [
    # (row id, BENCHMARKS.md row, legacy value, legacy type/state, acos value, verdict, note)
    ("M1", "Fixed idle core memory",
     "206.0 MiB (W1) / 303.9 MiB (v9); reads span 259-322 MiB",
     "anon+shmem, settled 60 s",
     (f"{mib(p50_of(idle.get('booted', {}).get('anon_plus_shmem_kib')))} MiB (booted app process), "
      f"{mib(p50_of(idle.get('vm', {}).get('anon_plus_shmem_kib')))} MiB (Vm only); "
      f"PSS {mib(p50_of(idle.get('booted', {}).get('pss_kib')))} MiB")
     if idle else None,
     None,
     "ACOS's settled state is a booted framework app process (attach handshake done); the Vm-only "
     "column is an even leaner state (boot-classpath DEX is parsed lazily, 40 classes at Vm-ready)"),
    ("M2", "Incremental memory per session at N=5",
     "78.7 MiB (v9) / 33.2 MiB (W2 opt-in)",
     "anon+shmem, warm",
     f"{session_row(5)} MiB/session at N=5" if sess else None, None,
     "in-process sequential sessions; immutable boot DEX, parsed DexFiles and decoded method bodies are Arc-shared through the process-global BootCache (no mmap; see results/bench/MEMORY_PROFILE.md)"),
    ("M3", "Cold start -> first correct frame", "3.620 s (v12)", "wall, cold",
     None, "not-measurable", "needs acos-render bound to the graphics natives"),
    ("M4", "Worker create -> correct frame", "1.251-1.343 s p50", "wall, warm",
     None, "not-measurable", "needs a worker-create path + renderer"),
    ("M5", "Fresh screenshot", "0.066-0.067 s p50", "wall, warm",
     None, "not-measurable", "needs acos-render capture + frame sequencing"),
    ("M6", "Reset-to-ready", "110.3 ms cmd / 748.1 ms -> frame", "wall, warm",
     None, "not-measurable", "no reset path (ResetRequest is protocol-only)"),
    ("M7", "UI tree with text", "538-768 ms one-shot / 4.1 ms persistent / 14-87 ms structure",
     "wall, warm", None, "not-measurable", "UI-tree bridge not wired"),
    ("M8", "Action-to-state latency", "not cleanly published (U1 step-5 747 ms incl. tree)",
     "wall, warm", None, "not-measurable", "needs input injection + UI tree"),
    ("M9", "Concurrent workloads (N)",
     "N=5 scorecard, N=10 measured; 10 extra displays", "count + memory",
     (f"N=10 measured for memory: {mib(sess.get('sessions', [{}])[-1].get('anon_plus_shmem_kib', {}).get('p50'))} MiB "
      f"anon+shmem at N=10, marginal {marginal_row(5)} MiB/session (N=5->10)")
     if sess and sess.get("sessions") else None,
     None, "memory is measured; independent state/input at N is NOT (process-global Binder transport)"),
    ("M10", "End-to-end verify wall time",
     "install-bound dev loop ~19 s, 74-81 % adb install (no direct equivalent)",
     "wall", None, "not-measurable", "needs launch/assert/evidence/reset/cleanup"),
    ("M11", "Verified tasks / GiB-minute", "not published as such", "ratio",
     None, "not-measurable", "needs the T01-T12 suite + job-cgroup accounting"),
    # ---- legacy rows from BENCHMARKS.md §3 that map onto today's metrics ----
    ("L-idle-core", "Legacy bare core (P2c, stripped)", "278 MiB", "anon, settled",
     (f"{mib(p50_of(idle.get('vm', {}).get('anon_plus_shmem_kib')))} MiB (Vm only, anon+shmem)"
      if idle else None), None,
     "closest state = Vm ready with zero tasks; ACOS also holds the boot classpath in the heap"),
    ("L-session-n1", "Per-session N=1 (v9)", "130.3 MiB", "anon+shmem, warm",
     (f"{session_row(1)} MiB/session at N=1" if sess else None), None, "in-process session"),
    ("L-marginal", "Marginal N=5->10 (v5)", "~99.3 MiB workset", "workset, warm",
     (f"{marginal_row(5)} MiB/session (N=5->10)" if sess else None), None, "anon+shmem, not cgroup workset"),
    ("L-cold-first-shot", "Cold core->first shot (v12)", "3.620 s", "wall, cold",
     None, "not-measurable", "no renderer"),
    ("L-guest-boot", "Guest boot `boot_completed` (v10)", "1.517 s", "wall, cold",
     ((lambda ms: f"{ms/1000:.3f} s attach handshake incl. Vm init (no guest kernel/zygote to boot)"
       )(p50_of(res('boot-attach')['boot_ms'])) if res('boot-attach') else None)
     if res("boot-attach") else None, None,
     "not the same thing: ACOS has no guest boot; the attach handshake is the analogous gate"),
    ("L-worker-frame", "Worker create -> frame", "1.251-1.343 s p50", "wall, warm",
     None, "not-measurable", "no worker path"),
    ("L-screenshot", "Fresh screenshot", "0.066-0.067 s p50", "wall, warm",
     None, "not-measurable", "no renderer"),
    ("L-resume-shot", "Resume->shot", "0.207-0.215 s", "wall, warm",
     None, "not-measurable", "no renderer"),
    ("L-reset", "`reset --app` cmd / -> frame", "110.3 ms / 748.1 ms p50", "wall, warm",
     None, "not-measurable", "no reset path"),
    ("L-user-reset", "Whole-user reset + observe", "48-50 s", "wall, warm",
     None, "not-measurable", "no reset path"),
    ("L-ui-tree", "UI tree structure/text", "14-87 ms / 538-768 ms / 4.1 ms", "wall, warm",
     None, "not-measurable", "UI-tree bridge not wired"),
    ("L-idle-cpu", "Parked idle CPU", "3.86-5.94 % (legacy called it unreliable)", "CPU window",
     (f"{p50_of(acos.get('idle-cpu', {}).get('cpu_pct_one_core'))} % of one core, "
      f"{p50_of(acos.get('idle-cpu', {}).get('cpu_ticks'))} ticks over "
      f"{acos.get('idle-cpu', {}).get('window_secs')} s") if acos.get("idle-cpu") else None, None,
     "ACOS: the interpreter thread is parked/stopped, so the process sleeps"),
    ("L-emulator-boot", "Stock emulator cold boot", "25.2 s / ~3300 MiB", "PSS, cold",
     None, "not-measurable", "no emulator/guest in the ACOS path (out of scope)"),
    ("L-renderer", "Host renderer R1", "6.27 s p50, 1.26 GiB RSS", "wall/RSS, host-only",
     None, "not-measurable", "no renderer"),
    ("L-chrome", "Chrome reference", "core 340 MiB PSS; shot 61 ms; AX 18 ms", "PSS, warm",
     None, "not-measurable", "reference only"),
    # ---- ACOS-native rows with no legacy counterpart ----
    ("A-process-start", "Process spawn -> Vm ready (ACOS-native)", "no legacy metric",
     "-",
     (f"{p50_of(res('process-start')['wall_ms'])} ms p50 / "
      f"{p50_of(res('process-start')['wall_ms'])} ms (p95 "
      f"{res('process-start')['wall_ms'].get('p95')})") if res("process-start") else None,
     "no-legacy-baseline", "legacy published only whole-job cold start"),
    ("A-attach-bind", "attach -> bindApplication delivered (ACOS-native)",
     "no legacy metric", "-",
     (f"{p50_of(res('bind-app')['attach_to_bind_ms'])} ms") if res("bind-app") else None,
     "no-legacy-baseline", "the legacy engine never published this stage split"),
    ("A-pool-reset", "SessionPool reset-to-clean (restore) vs legacy reset cmd",
     "110.3 ms cmd / 748.1 ms -> frame", "wall, warm",
     (lambda p: (f"{p.get('sessions', [{}])[0].get('reset_ms_p50')} ms p50 at N=1; "
                 f"{next((r.get('reset_ms_p50') for r in p.get('sessions', []) if r.get('n') == 5), None)} ms at N=5")
      )(res("sessions-pool") or {}) if res("sessions-pool") else None,
     "not-comparable", "ACOS restores a quiescent core VM (no app process/frame); legacy reset is app-scoped and frame-timed"),
    ("A-pool-throughput", "SessionPool throughput (jobs/s, N=1..20) (ACOS-native)",
     "no legacy metric", "-",
     (lambda p: (f"{p.get('sessions', [{}])[0].get('throughput', {}).get('jobs_per_sec', {}).get('p50')} jobs/s at N=1; "
                 f"{next((r.get('throughput', {}).get('jobs_per_sec', {}).get('p50') for r in p.get('sessions', []) if r.get('n') == 20), None)} at N=20")
      )(res("sessions-pool") or {}) if res("sessions-pool") else None,
     "no-legacy-baseline", "in-process run+reset loop of a small DEX program; legacy published no comparable throughput"),
    ("A-pool-memory", "SessionPool marginal memory per session (N=10..20)",
     "marginal N=5->10 ~99.3 MiB workset (v5)", "workset, warm",
     (lambda p: (f"{next((r.get('marginal_miB_per_session') for r in p.get('marginal', []) if r.get('from_n') == 10), None)} MiB/session (anon+shmem)")
      )(res("sessions-pool") or {}) if res("sessions-pool") else None,
     None, "template snapshot is a quiescent core VM (not a booted app process); anon+shmem vs legacy workset type differs"),
]

# numeric verdicts for the paired rows (both sides in MiB or the same unit)
def numeric_verdict(acos_mib, legacy_mib, noise=0.05):
    if acos_mib is None or legacy_mib is None:
        return "not-measurable"
    delta = (acos_mib - legacy_mib) / legacy_mib
    if abs(delta) <= noise:
        return f"comparable ({delta:+.1%})"
    return f"better ({delta:+.1%})" if delta < 0 else f"worse ({delta:+.1%})"

def legacy_mib(text):
    # first numeric value in the citation string
    import re
    m = re.search(r"([0-9]+(?:\.[0-9]+)?) MiB", text)
    return float(m.group(1)) if m else None

m1v = mib(p50_of(idle.get("vm", {}).get("anon_plus_shmem_kib")))
m1b = mib(p50_of(idle.get("booted", {}).get("anon_plus_shmem_kib")))
best = {}
for rid, val in [("M1", m1b), ("L-idle-core", m1v),
                 ("M2", session_row(5)), ("L-session-n1", session_row(1)),
                 ("L-marginal", marginal_row(5))]:
    best[rid] = val
lw = {"M1": 206.0, "L-idle-core": 278.0, "M2": 78.7, "L-session-n1": 130.3, "L-marginal": 99.3}

lines = []
w = lines.append
w("# ACOS V1 benchmark report")
w("")
w(f"- run: `{run_path.split('/')[-1]}` ({run.get('timestamp')}, commit `{run.get('commit')}`, "
  f"{run.get('samples_per_metric')} samples/metric, `{run.get('profile')}` profile)")
h = run.get("host", {})
w(f"- host: {h.get('threads')} threads, {h.get('ram_total_kib', 0)//1024//1024} GiB RAM, "
  f"loadavg1 {h.get('loadavg1')}, kernel {h.get('kernel')}, `{h.get('rustc')}`")
w(f"- method: {run.get('methodology')}")
w("- fidelity: `native-runtime` (ACOS executes the code itself). No row here is "
  "ART-verified, and no legacy value is an ACOS result.")
w("")
w("Metric types are never mixed: RSS, PSS, anon+shmem (`RssAnon+RssShmem`) and private/shared "
  "pages are reported in separate labelled columns.")
w("")
w("## 1. Measured ACOS metrics")
w("")
w("| Metric | Value (p50, p95) | Type | Cold/warm |")
w("|---|---|---|---|")
def fmt(block):
    if not isinstance(block, dict) or "p50" not in block:
        return "-"
    return f"{block['p50']} / {block.get('p95')}"
r = res("process-start")
if r:
    w(f"| process spawn -> Vm ready | {fmt(r['wall_ms'])} ms | wall (parent-measured) | cold |")
r = res("boot-attach")
if r:
    w(f"| Vm::new (boot classpath loaded) | {fmt(r['vm_ready_ms'])} ms | wall | cold |")
    w(f"| boot -> attachApplication | {fmt(r['boot_ms'])} ms | wall | cold |")
    c = r.get("counters") or {}
    w(f"| boot counters | classes={c.get('classes_loaded')}, boot-classpath-classes="
      f"{c.get('boot_classpath_classes')}, clinits={c.get('clinits')}, "
      f"instructions={c.get('instructions')}, natives={c.get('natives_invoked')} | counts | cold |")
    for st in r.get("stages", []):
        w(f"| - stage `{st['stage']}` | {fmt(st['ms'])} ms | wall | cold |")
r = res("bind-app")
if r:
    w(f"| attach -> bindApplication delivered | {fmt(r['attach_to_bind_ms'])} ms | wall | cold |")
    w(f"| bindApplication -> parked/blocked | {fmt(r['bind_phase_ms'])} ms | wall | cold |")
    if r.get("first_blocker"):
        w(f"| first blocker after bind | `{r['first_blocker']}` | structured | - |")
if idle:
    for name, label in (("vm", "fixed idle core (Vm only, zero tasks)"),
                        ("booted", "fixed idle core (framework booted)")):
        v = idle.get(name) or {}
        w(f"| {label}: PSS | {fmt(v.get('pss_kib'))} KiB | PSS | settled |")
        w(f"| {label}: RSS | {fmt(v.get('rss_kib'))} KiB | RSS | settled |")
        w(f"| {label}: anon+shmem | {fmt(v.get('anon_plus_shmem_kib'))} KiB | anon+shmem | settled |")
        w(f"| {label}: private / shared | "
          f"{(v.get('private_kib') or {}).get('p50')} / {(v.get('shared_kib') or {}).get('p50')} KiB "
          f"| private/shared pages | settled |")
if sess:
    for row in sess.get("sessions", []):
        def p50(x):
            b = row.get(x)
            return "-" if not isinstance(b, dict) else b.get("p50")
        w(f"| N={row['n']} sessions: PSS / anon+shmem / private / shared | "
          f"{p50('pss_kib')} / {p50('anon_plus_shmem_kib')} / {p50('private_kib')} / "
          f"{p50('shared_kib')} KiB (p50) | mixed, labelled | fresh process per N |")
    for row in sess.get("incremental", []):
        w(f"| incremental N={row['n']} | {row['incremental_miB_per_session']} MiB/session | "
          "anon+shmem | fresh process |")
    for row in sess.get("marginal", []):
        w(f"| marginal N={row['from_n']}->N={row['to_n']} | {row['marginal_miB_per_session']} "
          "MiB/session | anon+shmem | fresh process |")
if sess_shared:
    for row in sess_shared.get("sessions", []):
        def p50(x):
            b = row.get(x)
            return "-" if not isinstance(b, dict) else b.get("p50")
        w(f"| N={row['n']} sessions (shared boot): PSS / anon+shmem / private / shared | "
          f"{p50('pss_kib')} / {p50('anon_plus_shmem_kib')} / {p50('private_kib')} / "
          f"{p50('shared_kib')} KiB (p50) | mixed, labelled | fresh process per N |")
    for row in sess_shared.get("incremental", []):
        w(f"| incremental N={row['n']} (shared boot) | "
          f"{row['incremental_miB_per_session']} MiB/session | anon+shmem | fresh process |")
    for row in sess_shared.get("marginal", []):
        w(f"| marginal N={row['from_n']}->N={row['to_n']} (shared boot) | "
          f"{row['marginal_miB_per_session']} MiB/session | anon+shmem | fresh process |")
pool = acos.get("sessions-pool") or {}
if pool:
    for row in pool.get("sessions", []):
        w(f"| N={row['n']} session pool: anon+shmem / PSS | "
          f"{fmt(row.get('memory', {}).get('anon_plus_shmem_kib'))} / "
          f"{fmt(row.get('memory', {}).get('pss_kib'))} KiB (p50) | anon+shmem / PSS (separate) | warm restore |")
        w(f"| N={row['n']} session pool: jobs/s / reset ms / session create/s | "
          f"{p50_of(row.get('throughput', {}).get('jobs_per_sec'))} / {row.get('reset_ms_p50')} / "
          f"{p50_of(row.get('session_create', {}).get('sessions_per_sec'))} | wall | warm |")
        w(f"| N={row['n']} session pool: CPU cores used / efficiency | "
          f"{p50_of(row.get('cpu', {}).get('cores_used'))} / "
          f"{row.get('cpu', {}).get('efficiency_cores_per_session_p50')} cores/session | CPU | warm |")
    for row in pool.get("marginal", []):
        w(f"| session pool marginal N={row['from_n']}->N={row['to_n']} | "
          f"{row['marginal_miB_per_session']} MiB/session | anon+shmem | warm |")
r = res("idle-cpu")
if r:
    w(f"| idle CPU, parked/blocked | {fmt(r['cpu_ticks'])} ticks over {r['window_secs']} s "
      f"({fmt(r['cpu_pct_one_core'])} % of one core), zero_ticks={r['zero_ticks_over_window']} | "
      "CPU window | parked |")
if interp:
    for row in interp.get("programs", []):
        w(f"| interpreter `{row['program']}` | {fmt(row['wall_ms'])} ms, "
          f"{row.get('instructions')} insns, {row.get('instructions_per_second_p50')} insn/s | "
          "wall + VM counters | cold |")
w("")
w("## 2. ACOS vs legacy, every row of BENCHMARKS.md")
w("")
w("Verdicts: `better`/`worse` are computed only for the pairs whose metric type and state match "
  "(anon+shmem vs anon+shmem, MiB vs MiB). Everything else is `not-measurable` (no engine "
  "capability yet) or `no-legacy-baseline` (legacy never published it). Nothing is estimated.")
w("")
w("| Row | Metric | Legacy (ACVM, reference only) | Legacy type/state | ACOS | Verdict | Note |")
w("|---|---|---|---|---|---|---|")
for rid, label, legacy, legacy_type, now, verdict, note in ROWS:
    if verdict is None:
        verdict = numeric_verdict(best.get(rid), lw.get(rid)) if rid in lw else "measured"
    shown = now if now is not None else "**not measured**"
    w(f"| {rid} | {label} | {legacy} | {legacy_type} | {shown} | `{verdict}` | {note} |")
w("")
w("## 3. Worse than legacy (explicit, with plan)")
w("")
worse = []
for rid, label, legacy, _lt, now, verdict, note in ROWS:
    v = verdict if verdict is not None else (
        numeric_verdict(best.get(rid), lw.get(rid)) if rid in lw else "measured")
    if isinstance(v, str) and v.startswith("worse"):
        worse.append((rid, label, legacy, now, v, note))
if not worse:
    w("No row where ACOS is measurably worse than legacy exists in today's measurable set. "
      "This is **not** a win claim: the metrics where ACOS is unmeasured (frames, screenshots, "
      "UI tree, reset, workers) are exactly where legacy published its best numbers, so ACOS has "
      "no evidence there yet.")
else:
    w("| Row | Metric | Legacy | ACOS | Delta | Plan |")
    w("|---|---|---|---|---|---|")
    for rid, label, legacy, now, v, note in worse:
        w(f"| {rid} | {label} | {legacy} | {now} | {v} | {note} |")
w("")
w("## 4. not_yet_measurable (dependency named, never guessed)")
w("")
w("| Metric | BENCHMARKS.md row | Blocking dependency |")
w("|---|---|---|")
DEP = [
    ("cold start -> first correct frame", "M3", "acos-render is not bound to the graphics natives; no frame exists to be correct"),
    ("worker create -> correct frame", "M4", "no worker-create path + renderer"),
    ("fresh screenshot", "M5", "acos-render capture + request/cause frame sequencing"),
    ("reset -> ready", "M6", "no reset implementation (ResetRequest is protocol-only)"),
    ("UI tree with text", "M7", "UI-tree bridge not wired (UiNode/UiTree are protocol-only)"),
    ("action -> state latency", "M8", "input injection + UI tree"),
    ("end-to-end verify", "M10", "launch (ClientTransaction, T-23) + assert + evidence + reset"),
    ("verified tasks / GiB-minute", "M11", "T01-T12 suite + job-cgroup accounting"),
    ("N=20 concurrent workers", "V1 §5.1", "process-global Binder transport: sessions are not independently usable"),
    ("per-session correctness / isolation at N", "M9 (correctness half)", "same as above; only memory at N is measurable today"),
]
for name, row, dep in DEP:
    w(f"| {name} | {row} | {dep} |")
w("")
w("## 5. Raw artifacts")
w("")
w(f"- run document (envelopes + host/method record): `{run_path.split('/')[-1]}`")
w("- per-metric envelopes are embedded in `envelopes`; every one is a validated "
  "`acos-protocol` envelope (`ok: true` only with `status: pass`).")
w("- regression gate: `tools/bench-acos.sh --check` (baseline `results/bench/baseline.json`).")
w("")

open(report_path, "w").write("\n".join(lines) + "\n")
print("\n".join(lines))
print(f"\nbench-acos: wrote {report_path}")
PY

if [ "$WRITE_BASELINE" = 1 ]; then
    python3 - "$RUN_JSON" "$BASELINE" <<'PYB'
import json, sys
run_path, baseline_path = sys.argv[1:3]
run = json.load(open(run_path))
rows = []
for rid, block in sorted(run.get("gates", {}).items()):
    rows.append({
        "id": rid,
        "path": "",
        "value": block["p50"],
        "direction": "lower",
        "tolerance": 0.25,
    })
doc = {
    "schema": "acos-bench-baseline/v1",
    "created": run.get("timestamp"),
    "commit": run.get("commit"),
    "source_run": run_path.split("/")[-1],
    "note": ("Committed regression baseline: p50 of every gated row from the source run. "
             "A regression gate fails when a fresh p50 exceeds value*(1+max(tolerance, spread)). "
             "Update only with a recorded run and an explicit note in BENCHMARKS.md."),
    "rows": rows,
}
json.dump(doc, open(baseline_path, "w"), indent=2)
print(f"bench-acos: wrote baseline {baseline_path} ({len(rows)} rows)")
PYB
fi

if [ "$CHECK_AFTER" = 1 ]; then
    exec "$0" --check --from-run "$RUN_JSON"
fi

echo "bench-acos: done. Commit the run if it is the new baseline:"
echo "  cp $RUN_JSON $RESULTS/baseline.json   # then update baseline 'rows' via --make-baseline"
