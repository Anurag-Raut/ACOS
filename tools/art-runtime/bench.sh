#!/usr/bin/env bash
#
# bench.sh -- headline benchmark: correctly verified Android dev tasks per GiB-minute
# at N concurrent independent agents (docs/ENGINEERING_POLICY.md "Benchmarks").
#
#   tools/art-runtime/bench.sh <rust-vm|art> [--ns "1 5 10"] [--reps 5] [--out DIR] [--no-build]
#
# Engines
#   rust-vm  ACOS Rust VM: one `acos serve` daemon per agent, `acos launch --keep`,
#            driven through `acos observe|tap|session reset` (session label s_cli).
#   art      Real ART (acos-art): one `acos-art session` process per agent, driven over
#            its line-JSON unix socket (screenshot|tree|tap|reset|close). The one-shot
#            `acos-art render-app` is measured as a separate row.
#
# Workload (every agent, identical): fixtures/build/apps/counter.apk
#   launch -> first correct frame (tree shows count "0")
#   REPS x (screenshot, tree) ; 3 x (tap Increment, tree shows count k)
#   reset (rust-vm: drops the VM and relaunches; art: fresh Activity) -> tree shows "0"
# A session is a VERIFIED task only if every step's check passes. Nothing is taken
# from an engine's own "ok" alone: state is read back from the UI tree.
#
# Memory: sums of per-process /proc/<pid>/smaps_rollup over each agent's process tree
# (Pss, Private_Dirty, Rss). PSS sums are attributable totals (shared pages divided by
# sharers); never summed with cgroup numbers. Host used = MemTotal - MemAvailable.
# Integral for tasks/GiB-min = trapezoid over PSS samples (PSS, not cgroup; labelled).
#
# Output (scratch, gitignored): target/art-runtime-bench/<engine>-<UTC stamp>/
#   results.json, table.md, logs/, shots/ . Table is also printed to stdout.
#
# Env overrides:
#   ACOS_BENCH_MEM      systemd MemoryMax for the whole run (default 16G)
#   ACOS_BENCH_TIME     wall-clock ceiling for the whole run in seconds (default 2400)
#   ACOS_BENCH_RAM_MAX  do not start a level above this host used % (default 85)
#   ACOS_BENCH_RAM_HARD abort a running level above this host used % (default 90)
#   ACOS_BENCH_COLD=0   skip the cold control (rust-vm, boot cache off)
set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
ENGINE="${1:-}"
shift || true
NS="1 5 10"
REPS=5
OUT=""
NO_BUILD=0
while [ $# -gt 0 ]; do
    case "$1" in
        --ns)       NS="$2"; shift 2 ;;
        --reps)     REPS="$2"; shift 2 ;;
        --out)      OUT="$2"; shift 2 ;;
        --no-build) NO_BUILD=1; shift ;;
        -h|--help)  sed -n '2,36p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'; exit 0 ;;
        *) echo "bench.sh: unknown argument: $1" >&2; exit 2 ;;
    esac
done

case "$ENGINE" in
    rust-vm|art) ;;
    *) echo "usage: tools/art-runtime/bench.sh <rust-vm|art> [--ns \"1 5 10\"] [--reps 5] [--out DIR]" >&2; exit 2 ;;
esac

MEM="${ACOS_BENCH_MEM:-16G}"
TIME="${ACOS_BENCH_TIME:-2400}"
RAM_MAX="${ACOS_BENCH_RAM_MAX:-85}"
RAM_HARD="${ACOS_BENCH_RAM_HARD:-90}"
COLD="${ACOS_BENCH_COLD:-1}"
STAMP="$(date -u +%Y%m%dT%H%M%SZ)"
[ -n "$OUT" ] || OUT="$ROOT/target/art-runtime-bench/$ENGINE-$STAMP"
mkdir -p "$OUT/logs" "$OUT/shots" "$OUT/run" "$OUT/cache"

# Hold stdin open for the whole run. With a closed/EOF stdin, acos-art sessions shut down
# with SIGTERM mid-run (observed under guarded.sh; a pipe that stays open does not).
HOLD_FIFO="$OUT/run/stdin.hold"
mkfifo "$HOLD_FIFO"
sleep "${ACOS_BENCH_TIME:-2400}" >"$HOLD_FIFO" &
HOLD_PID=$!
exec 0<"$HOLD_FIFO"
trap 'kill "$HOLD_PID" 2>/dev/null' EXIT

APK="$ROOT/fixtures/build/apps/counter.apk"
if [ ! -f "$APK" ]; then
    echo "bench.sh: $APK missing (run tools/build-apps.sh)" >&2
    exit 2
fi

ACOS="$ROOT/target/release/acos"
ACOS_ART="$ROOT/target/release/acos-art"

# ---- build (guarded). A failed build is reported and the engine is skipped. ----
if [ "$NO_BUILD" -eq 0 ]; then
    if [ "$ENGINE" = "rust-vm" ]; then
        BUILD_ARGS=(-p acos-cli)
    else
        BUILD_ARGS=(-p acos-art)
    fi
    if ! CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-4}" "$ROOT/tools/guarded.sh" --mem 12G --time 900 -- \
            cargo build --release "${BUILD_ARGS[@]}" >"$OUT/logs/build.log" 2>&1; then
        echo "bench.sh: build of ${BUILD_ARGS[*]} failed; see $OUT/logs/build.log" >&2
        printf '{"engine":"%s","status":"skipped: build failed","log":"%s"}\n' "$ENGINE" "$OUT/logs/build.log" >"$OUT/results.json"
        exit 3
    fi
fi
if [ "$ENGINE" = "art" ] && [ ! -x "$ACOS_ART" ]; then
    echo "bench.sh: $ACOS_ART not built" >&2
    exit 3
fi
if [ "$ENGINE" = "rust-vm" ] && [ ! -x "$ACOS" ]; then
    echo "bench.sh: $ACOS not built" >&2
    exit 3
fi

# ---- run the measurement (python driver, bounded by one guarded scope) ----
# The driver is written to a file first: guarded.sh runs its child in the background,
# and a background job's stdin is /dev/null, so `python3 -` would read an empty program.
cat >"$OUT/driver.py" <<'PY'
import datetime
import hashlib
import json
import os
import re
import signal
import socket
import statistics
import subprocess
import sys
import threading
import time

(ENGINE, ROOT, OUT, NS_ARG, REPS_ARG, COLD_ARG, RAM_MAX_ARG, RAM_HARD_ARG,
 TIME_ARG, APK, ACOS, ACOS_ART) = sys.argv[1:13]
NS = [int(x) for x in NS_ARG.split()]
REPS = int(REPS_ARG)
COLD = COLD_ARG == "1"
RAM_MAX = float(RAM_MAX_ARG)
RAM_HARD = float(RAM_HARD_ARG)
RUN = os.path.join(OUT, "run")
LOGS = os.path.join(OUT, "logs")
SHOTS = os.path.join(OUT, "shots")
CACHE = os.path.join(OUT, "cache")
IDLE = int(TIME_ARG)
COUNT_SUFFIX = ":id/count"
TAPS = 3


def now():
    return time.perf_counter()


def ms(a, b):
    return (b - a) * 1000.0


# ---------------------------------------------------------------- host / memory
def meminfo():
    m = {}
    with open("/proc/meminfo") as f:
        for line in f:
            k, v = line.split(":", 1)
            m[k] = int(v.split()[0])  # kB
    total, avail = m["MemTotal"], m["MemAvailable"]
    used_mib = (total - avail) / 1024.0
    return used_mib, 100.0 * (total - avail) / total, total / 1024.0


def ppid_map():
    pm = {}
    for d in os.listdir("/proc"):
        if not d.isdigit():
            continue
        try:
            with open(f"/proc/{d}/stat") as f:
                s = f.read()
            ppid = int(s.rsplit(")", 1)[1].split()[1])
            pm.setdefault(ppid, []).append(int(d))
        except (OSError, ValueError, IndexError):
            pass
    return pm


def tree_pids(roots):
    pm = ppid_map()
    seen, stack = set(), list(roots)
    while stack:
        p = stack.pop()
        if p in seen:
            continue
        seen.add(p)
        stack.extend(pm.get(p, []))
    return seen


def rollup_kb(pid, key):
    try:
        with open(f"/proc/{pid}/smaps_rollup") as f:
            for line in f:
                if line.startswith(key + ":"):
                    return int(line.split()[1])
    except OSError:
        return 0
    return 0


def sum_mem(roots):
    pids = tree_pids(roots)
    pss = priv = rss = 0
    for p in pids:
        pss += rollup_kb(p, "Pss")
        priv += rollup_kb(p, "Private_Dirty")
        rss += rollup_kb(p, "Rss")
    return pss / 1024.0, priv / 1024.0, rss / 1024.0, len(pids)


def stats(xs):
    xs = sorted(x for x in xs if x is not None)
    if not xs:
        return None
    def pct(p):
        return xs[min(len(xs) - 1, int(round(p / 100.0 * (len(xs) - 1))))]
    return {"n": len(xs), "p50": round(statistics.median(xs), 2), "p95": round(pct(95), 2),
            "min": round(xs[0], 2), "max": round(xs[-1], 2)}


# ---------------------------------------------------------------- process helpers
def run_cmd(args, env, timeout=300):
    t = now()
    try:
        r = subprocess.run(args, cwd=ROOT, env=env, capture_output=True, text=True, timeout=timeout)
        rc, out, err = r.returncode, r.stdout, r.stderr
    except subprocess.TimeoutExpired:
        rc, out, err = 124, "", "timeout"
    return ms(t, now()), rc, out, err


def parse_json(s):
    try:
        return json.loads(s)
    except Exception:
        return None


def find_count(nodes):
    """Walk a UI tree (rust-vm `resource_id`, art `id`) and return the count label text."""
    stack = list(nodes or [])
    while stack:
        n = stack.pop()
        if not isinstance(n, dict):
            continue
        rid = n.get("resource_id") or n.get("id") or ""
        if isinstance(rid, str) and rid.endswith(COUNT_SUFFIX):
            return n.get("text")
        stack.extend(n.get("children") or [])
    return None


def find_center(nodes, text):
    stack = list(nodes or [])
    while stack:
        n = stack.pop()
        if not isinstance(n, dict):
            continue
        if n.get("text") == text:
            b = n.get("bounds")
            if isinstance(b, list):
                l, t, r, bt = b
            else:
                l, t, r, bt = [int(v) for v in re.findall(r"-?\d+", str(b))][:4]
            return (l + r) // 2, (t + bt) // 2
        stack.extend(n.get("children") or [])
    return None


def wait_for(pred, timeout):
    end = time.time() + timeout
    while time.time() < end:
        if pred():
            return True
        time.sleep(0.02)
    return False


# ---------------------------------------------------------------- rust-vm engine
class RustVM:
    kind = "rust-vm"

    def __init__(self, label, cold=False):
        self.label = label
        self.cold = cold
        self.sock = os.path.join(RUN, f"{label}.sock")
        self.env = dict(os.environ, ACOS_SESSION_SOCKET=self.sock, ACOS_CACHE_DIR=CACHE)
        if cold:
            self.env["ACOS_NO_BOOT_CACHE"] = "1"
        self.proc = None
        self.t0 = None
        self.launch_ms = None
        self.first_ok = False
        self.first_frame_ms = None

    def roots(self):
        return [self.proc.pid] if self.proc and self.proc.poll() is None else []

    def start_daemon(self):
        log = open(os.path.join(LOGS, f"{self.label}-serve.log"), "w")
        self.proc = subprocess.Popen(
            [ACOS, "serve", "--socket", self.sock, "--idle-timeout", str(IDLE), "--max-sessions", "2"],
            cwd=ROOT, env=self.env, stdout=log, stderr=subprocess.STDOUT, start_new_session=True)
        if not wait_for(lambda: os.path.exists(self.sock), 30):
            raise RuntimeError("serve socket never appeared")

    def launch(self):
        self.t0 = now()
        self.start_daemon()
        dt, rc, out, err = run_cmd([ACOS, "launch", APK, "--keep", "--json"], self.env, 300)
        d = parse_json(out) or {}
        self.launch_ms = ms(self.t0, now())
        res = d.get("result") or {}
        self.first_ok = rc == 0 and (res.get("reached_stage") or res.get("stage_reached")) == "first_frame"
        self.first_frame_ms = self.launch_ms
        self.launch_rtt_ms = dt
        return self.first_ok

    def tree(self):
        dt, rc, out, _ = run_cmd([ACOS, "observe", "--tree", "--session", "s_cli", "--json"], self.env)
        d = parse_json(out) or {}
        nodes = ((d.get("result") or {}).get("tree") or {}).get("nodes")
        return dt, rc == 0 and nodes is not None, find_count(nodes), nodes

    def shot(self):
        p = os.path.join(SHOTS, f"{self.label}.png")
        dt, rc, _, _ = run_cmd([ACOS, "observe", "--screenshot", p, "--session", "s_cli", "--json"], self.env)
        return dt, rc == 0 and os.path.exists(p) and os.path.getsize(p) > 0

    def tap(self):
        dt, rc, out, _ = run_cmd([ACOS, "tap", "--text", "Increment", "--session", "s_cli", "--json"], self.env)
        res = (parse_json(out) or {}).get("result") or {}
        return dt, rc == 0 and bool(res.get("accepted")), bool(res.get("state_changed"))

    def reset(self):
        # `session reset` drops the VM (state_reset=true) but does not relaunch, so reset-to-ready
        # is reset + relaunch to a fresh first frame. Scope = whole task (VM).
        dt1, rc1, out1, _ = run_cmd([ACOS, "session", "reset", "--session", "s_cli", "--json"], self.env, 300)
        reset_ok = rc1 == 0 and bool((parse_json(out1) or {}).get("result", {}).get("state_reset"))
        dt2, rc2, out2, _ = run_cmd([ACOS, "launch", APK, "--keep", "--json"], self.env, 300)
        res = (parse_json(out2) or {}).get("result") or {}
        relaunch_ok = rc2 == 0 and (res.get("reached_stage") or res.get("stage_reached")) == "first_frame"
        return dt1 + dt2, reset_ok and relaunch_ok

    def stop(self):
        run_cmd([ACOS, "session", "destroy", "--session", "s_cli"], self.env, 60)
        if self.proc and self.proc.poll() is None:
            try:
                os.killpg(self.proc.pid, signal.SIGTERM)
                self.proc.wait(10)
            except Exception:
                os.killpg(self.proc.pid, signal.SIGKILL)
                self.proc.wait()


# ---------------------------------------------------------------- art engine
class Art:
    kind = "art"

    def __init__(self, label):
        self.label = label
        self.sid = label
        self.sock = os.path.join(RUN, f"{label}.sock")
        self.outdir = os.path.join(OUT, "art-sessions", label)
        self.proc = None
        self.conn = None
        self.buf = b""
        self.t0 = None
        self.launch_ms = None
        self.first_ok = False
        self.first_frame_ms = None
        self.launch_rtt_ms = None

    def roots(self):
        return [self.proc.pid] if self.proc and self.proc.poll() is None else []

    def launch(self):
        os.makedirs(self.outdir, exist_ok=True)
        log = open(os.path.join(LOGS, f"{self.label}.log"), "w")
        self.t0 = now()
        # stdin is a pipe we keep open for the session's life: under guarded.sh the driver's
        # stdin is /dev/null, and acos-art treats stdin EOF as a shutdown (SIGTERM to itself).
        self.proc = subprocess.Popen(
            [ACOS_ART, "session", "--apk", APK, "--out", self.outdir, "--session-id", self.sid,
             "--socket", self.sock, "--timeout", str(IDLE)],
            cwd=ROOT, stdin=subprocess.PIPE, stdout=log, stderr=subprocess.STDOUT, start_new_session=True)
        deadline = time.time() + 180
        while True:
            if self.proc.poll() is not None:
                raise RuntimeError(f"acos-art session exited rc={self.proc.returncode}")
            c = socket.socket(socket.AF_UNIX)
            c.settimeout(180)
            try:
                c.connect(self.sock)
                self.conn = c
                break
            except (FileNotFoundError, ConnectionRefusedError):
                c.close()
                if time.time() > deadline:
                    raise RuntimeError("acos-art session socket never accepted")
                time.sleep(0.02)
        # The socket is bound only after the guest's ready line (frame 1 published).
        dt, reply = self.req({"cmd": "tree"})
        self.first_frame_ms = ms(self.t0, now())
        self.launch_ms = self.first_frame_ms
        self.launch_rtt_ms = dt
        self.first_ok = bool(reply.get("ok")) and find_count((reply.get("tree") or {}).get("nodes")) == "0"
        return self.first_ok

    def req(self, obj):
        t = now()
        self.conn.sendall((json.dumps(obj) + "\n").encode())
        while True:
            while b"\n" not in self.buf:
                d = self.conn.recv(1 << 20)
                if not d:
                    raise RuntimeError("acos-art session closed the socket")
                self.buf += d
            line, _, self.buf = self.buf.partition(b"\n")
            try:
                reply = json.loads(line.decode())
            except Exception:
                continue
            if reply.get("cmd") == obj["cmd"]:
                return ms(t, now()), reply

    def tree(self):
        dt, reply = self.req({"cmd": "tree"})
        nodes = (reply.get("tree") or {}).get("nodes")
        return dt, bool(reply.get("ok")) and nodes is not None, find_count(nodes), nodes

    def shot(self):
        dt, reply = self.req({"cmd": "screenshot"})
        png = reply.get("png")
        return dt, bool(reply.get("ok")) and bool(png) and os.path.exists(png) and os.path.getsize(png) > 0

    def tap(self, nodes):
        c = find_center(nodes, "Increment")
        if c is None:
            return 0.0, False, False
        dt, reply = self.req({"cmd": "tap", "x": c[0], "y": c[1]})
        return dt, bool(reply.get("ok")), None

    def reset(self):
        # Fresh Activity in the same process: scope = activity.
        dt, reply = self.req({"cmd": "reset"})
        return dt, bool(reply.get("ok"))

    def stop(self):
        try:
            if self.conn:
                self.req({"cmd": "close"})
        except Exception:
            pass
        if self.conn:
            self.conn.close()
        if self.proc and self.proc.poll() is None:
            try:
                os.killpg(self.proc.pid, signal.SIGTERM)
                self.proc.wait(15)
            except Exception:
                os.killpg(self.proc.pid, signal.SIGKILL)
                self.proc.wait()


# ---------------------------------------------------------------- one agent's task
def run_agent(h, rec, abort):
    """One independent agent: launch, verify, screenshot/tree loop, taps, reset, verify."""
    try:
        h.launch()
        rec["launch_ms"] = h.launch_ms
        rec["launch_rtt_ms"] = h.launch_rtt_ms
        rec["first_frame_ms"] = h.first_frame_ms
        rec["first_frame_ok"] = h.first_ok
        if not h.first_ok:
            rec["why"].append("first frame / count '0' not verified")
            return
        nodes = None
        for _ in range(REPS):
            if abort.is_set():
                rec["why"].append("aborted (host RAM)")
                return
            dt, ok = h.shot()
            rec["shot_ms"].append(dt)
            rec["shot_ok"].append(ok)
            dt, ok, cnt, nodes = h.tree()
            rec["tree_ms"].append(dt)
            rec["tree_ok"].append(ok and cnt == "0")
        for k in range(1, TAPS + 1):
            if abort.is_set():
                rec["why"].append("aborted (host RAM)")
                return
            if h.kind == "rust-vm":
                dt, ok, changed = h.tap()
            else:
                dt, ok, _ = h.tap(nodes)
            rec["tap_ms"].append(dt)
            rec["tap_ok"].append(ok)
            dt2, tok, cnt, nodes = h.tree()
            rec["tap_tree_ms"].append(dt2)
            rec["tap_state_verified"].append(tok and cnt == str(k))
        dt, ok = h.reset()
        rec["reset_ms"] = dt
        dt2, tok, cnt, nodes = h.tree()
        rec["reset_verified"] = ok and tok and cnt == "0"
    except Exception as e:  # record, never crash the level
        rec["why"].append(f"exception: {e}")
    finally:
        h.stop()
        rec["stopped"] = True


def new_rec():
    return {"launch_ms": None, "launch_rtt_ms": None, "first_frame_ms": None, "first_frame_ok": False,
            "shot_ms": [], "shot_ok": [], "tree_ms": [], "tree_ok": [], "tap_ms": [], "tap_ok": [],
            "tap_tree_ms": [], "tap_state_verified": [], "reset_ms": None, "reset_verified": False,
            "why": [], "stopped": False}


def session_ok(rec):
    return (rec["first_frame_ok"] and rec["reset_verified"] and not rec["why"]
            and rec["shot_ok"] and all(rec["shot_ok"]) and all(rec["tree_ok"])
            and len(rec["tap_state_verified"]) == TAPS and all(rec["tap_state_verified"])
            and all(rec["tap_ok"]))


# ---------------------------------------------------------------- one concurrency level
def run_level(n, make, label, cold=False):
    used_mib, used_pct, total_mib = meminfo()
    if used_pct >= RAM_MAX:
        return {"n": n, "status": f"skipped: host used {used_pct:.1f}% >= {RAM_MAX}% before start"}
    handles = [make(f"{label}{i}") for i in range(n)]
    recs = [new_rec() for _ in range(n)]
    abort = threading.Event()
    launched = threading.Event()
    ops_done = threading.Event()
    stop_sampling = threading.Event()
    samples = []  # (t, pss_mib, priv_mib, rss_mib, host_used_mib, host_used_pct, n_procs)
    base_used = used_mib

    def sampler():
        t_start = now()
        while not stop_sampling.is_set():
            roots = [r for h in handles for r in h.roots()]
            pss, priv, rss, npr = sum_mem(roots)
            hu, hp, _ = meminfo()
            samples.append((now() - t_start, pss, priv, rss, hu, hp, npr, launched.is_set(), ops_done.is_set()))
            if hp >= RAM_HARD:
                abort.set()
            time.sleep(0.25)

    th_s = threading.Thread(target=sampler, daemon=True)
    th_s.start()
    t_level = now()
    threads = []
    for h, rec in zip(handles, recs):
        threads.append(threading.Thread(target=run_agent, args=(h, rec, abort), daemon=True))
    for t in threads:
        t.start()
    # Mark "all launched" once every agent has a verified first frame or failed to launch.
    wait_for(lambda: all(r["first_frame_ok"] or r["why"] for r in recs), 600)
    launched.set()
    for t in threads:
        t.join()
    ops_done.set()
    time.sleep(1.0)
    stop_sampling.set()
    th_s.join()
    level_wall = ms(t_level, now())

    steady = [s for s in samples if s[7] and s[8] is False]
    if not steady:
        steady = [s for s in samples if s[7]] or samples
    pss_steady = statistics.median([s[1] for s in steady]) if steady else 0.0
    priv_steady = statistics.median([s[2] for s in steady]) if steady else 0.0
    host_steady = statistics.median([s[4] for s in steady]) if steady else base_used
    peak = max((s[1] for s in samples), default=0.0)
    peak_host = max((s[5] for s in samples), default=0.0)
    # Integral of PSS over time (GiB-minutes), trapezoid over the sample times.
    integral = 0.0
    for a, b in zip(samples, samples[1:]):
        dt_min = (b[0] - a[0]) / 60.0
        integral += ((a[1] + b[1]) / 2.0 / 1024.0) * dt_min
    ok_sessions = sum(1 for r in recs if session_ok(r))
    leftover = sum_mem([r for h in handles for r in h.roots()])[3]
    return {
        "n": n,
        "label": label,
        "cold": cold,
        "status": "ok",
        "level_wall_s": round(level_wall / 1000.0, 2),
        "pss_steady_mib": round(pss_steady, 2),
        "private_dirty_steady_mib": round(priv_steady, 2),
        "pss_peak_mib": round(peak, 2),
        "host_used_steady_mib": round(host_steady, 2),
        "host_used_delta_steady_mib": round(host_steady - base_used, 2),
        "host_used_peak_pct": round(peak_host, 2),
        "integral_pss_gib_min": round(integral, 4),
        "sessions_total": n,
        "sessions_verified": ok_sessions,
        "tasks_per_gib_min": round(ok_sessions / integral, 3) if integral > 0 else None,
        "leftover_procs_after_teardown": leftover,
        "aborted": abort.is_set(),
        "launch_ms": stats([r["launch_ms"] for r in recs]),
        "launch_rtt_ms": stats([r["launch_rtt_ms"] for r in recs]),
        "first_frame_ok": sum(1 for r in recs if r["first_frame_ok"]),
        "shot_ms": stats([x for r in recs for x in r["shot_ms"]]),
        "shot_verified": sum(1 for r in recs for x in r["shot_ok"] if x),
        "shot_total": sum(len(r["shot_ok"]) for r in recs),
        "tree_ms": stats([x for r in recs for x in r["tree_ms"]]),
        "tap_ms": stats([x for r in recs for x in r["tap_ms"]]),
        "tap_tree_ms": stats([x for r in recs for x in r["tap_tree_ms"]]),
        "tap_state_verified": sum(1 for r in recs for x in r["tap_state_verified"] if x),
        "tap_total": sum(len(r["tap_state_verified"]) for r in recs),
        "reset_ms": stats([r["reset_ms"] for r in recs if r["reset_ms"] is not None]),
        "reset_verified": sum(1 for r in recs if r["reset_verified"]),
        "agent_issues": [r["why"] for r in recs if r["why"]],
        "session_samples": len(samples),
    }


def make_rv_factory(cold):
    return lambda label: RustVM(label, cold=cold)


def make_art_factory():
    return lambda label: Art(label)


# ---------------------------------------------------------------- one-shot rows
def oneshot_rust(reps):
    env = dict(os.environ, ACOS_CACHE_DIR=CACHE)
    rows = []
    for i in range(reps):
        p = os.path.join(SHOTS, f"oneshot{i}.png")
        dt, rc, out, err = run_cmd([ACOS, "observe", "--apk", APK, "--screenshot", p, "--json"], env, 300)
        d = parse_json(out) or {}
        ok = rc == 0 and os.path.exists(p) and os.path.getsize(p) > 0
        rows.append({"wall_ms": dt, "ok": ok, "stage": (d.get("result") or {}).get("stage_reached")})
    return {"cmd": "acos observe --apk counter.apk --screenshot (launch + frame + PNG, one process)",
            "wall_ms": stats([r["wall_ms"] for r in rows]),
            "verified": sum(1 for r in rows if r["ok"] and r["stage"] == "first_frame"), "n": reps}


def oneshot_art(reps):
    rows = []
    for i in range(reps):
        d_out = os.path.join(OUT, "render-app", f"r{i}")
        os.makedirs(d_out, exist_ok=True)
        dt, rc, out, err = run_cmd([ACOS_ART, "render-app", "--apk", APK, "--out", d_out, "--json"], os.environ, 300)
        d = parse_json(out) or {}
        ok = rc == 0 and d.get("outcome") == "ok" and d.get("frame") and os.path.exists(d["frame"])
        rows.append({"wall_ms": dt, "guest_wall_ms": d.get("wall_ms"), "peak_rss_mb": d.get("peak_rss_mb"),
                     "md5": d.get("frame_md5"), "nodes": d.get("nodes"), "ok": bool(ok)})
    return {"cmd": "acos-art render-app --apk counter.apk --out DIR --json (one process, one frame)",
            "wall_ms": stats([r["wall_ms"] for r in rows]),
            "guest_wall_ms": stats([r["guest_wall_ms"] for r in rows if r["guest_wall_ms"] is not None]),
            "peak_rss_mb": stats([r["peak_rss_mb"] for r in rows if r["peak_rss_mb"] is not None]),
            "frame_md5_set": sorted({r["md5"] for r in rows if r["md5"]}),
            "tree_nodes": sorted({r["nodes"] for r in rows if r["nodes"] is not None}),
            "verified": sum(1 for r in rows if r["ok"]), "n": reps}


# ---------------------------------------------------------------- provenance
def git(*args):
    try:
        return subprocess.run(["git", *args], cwd=ROOT, capture_output=True, text=True, timeout=30).stdout.strip()
    except Exception:
        return ""


def sha256(path):
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def provenance():
    binp = ACOS if ENGINE == "rust-vm" else ACOS_ART
    used, pct, total = meminfo()
    with open("/proc/loadavg") as f:
        load = f.read().split()[:3]
    return {
        "utc": datetime.datetime.now(datetime.timezone.utc).isoformat(timespec="seconds"),
        "git_head": git("rev-parse", "HEAD"),
        "crates_dirty_files": len([l for l in git("status", "--porcelain", "crates").splitlines() if l.strip()]),
        "binary": os.path.relpath(binp, ROOT),
        "binary_sha256_16": sha256(binp)[:16],
        "binary_mtime_utc": datetime.datetime.fromtimestamp(os.path.getmtime(binp), datetime.timezone.utc).isoformat(timespec="seconds"),
        "apk": os.path.relpath(APK, ROOT),
        "apk_sha256_16": sha256(APK)[:16],
        "host": {"mem_total_mib": round(total, 1), "mem_used_pct_at_start": round(pct, 2),
                 "loadavg_1_5_15": load, "nproc": os.cpu_count()},
        "fidelity": ("native-runtime (ACOS Rust VM executes the code; NOT ART-verified)"
                     if ENGINE == "rust-vm" else
                     "real-art (dalvikvm64 from API-34 APEX in bwrap mini-root; NOT a device/emulator run)"),
        "memory_type": "PSS sum over each agent's process tree (smaps_rollup); host = MemTotal-MemAvailable",
    }


# ---------------------------------------------------------------- main
def main():
    result = {"engine": ENGINE, "schema": "art-runtime-bench/1", "provenance": provenance(),
              "workload": "fixtures/build/apps/counter.apk: launch -> first frame (count 0), "
                          f"{REPS}x screenshot+tree, {TAPS} taps verified by tree, reset verified",
              "levels": [], "core": None, "oneshot": None, "unsupported": []}
    if ENGINE == "rust-vm":
        # Warm the boot-image cache once in a throwaway daemon (not recorded).
        warm = RustVM("warm")
        try:
            warm.start_daemon()
            run_cmd([ACOS, "launch", APK, "--keep", "--json"], warm.env, 300)
        finally:
            warm.stop()
        # Fixed core: a fresh daemon with ZERO sessions, boot cache warm.
        core = RustVM("core")
        try:
            core.start_daemon()
            time.sleep(3)
            pss, priv, rss, npr = sum_mem(core.roots())
            result["core"] = {"pss_mib": round(pss, 2), "private_dirty_mib": round(priv, 2),
                              "rss_mib": round(rss, 2), "procs": npr,
                              "note": "fresh idle `acos serve` daemon, zero sessions, boot cache warm"}
        finally:
            core.stop()
        if COLD:
            lvl = run_level(1, make_rv_factory(True), "cold", cold=True)
            lvl["note"] = "cold control: ACOS_NO_BOOT_CACHE=1 (first launch, no boot-image cache)"
            result["levels"].append(lvl)
        for n in NS:
            result["levels"].append(run_level(n, make_rv_factory(False), f"n{n}r"))
        result["oneshot"] = oneshot_rust(max(REPS, 3))
    else:
        result["core"] = {"pss_mib": 0.0, "note": "not applicable: no resident core; every agent is its own "
                                                 "acos-art session process (bwrap + dalvikvm64)"}
        result["unsupported"].append({"row": "cold control (boot cache off)",
                                      "reason": "no ART boot-cache switch; launch is always the session path"})
        for n in NS:
            result["levels"].append(run_level(n, make_art_factory(), f"n{n}a"))
        result["oneshot"] = oneshot_art(max(REPS, 3))

    with open(os.path.join(OUT, "results.json"), "w") as f:
        json.dump(result, f, indent=2, sort_keys=True)
    table = render_table(result)
    with open(os.path.join(OUT, "table.md"), "w") as f:
        f.write(table)
    print(table)
    print(f"results: {os.path.join(OUT, 'results.json')}")


def fmt(v, digits=1):
    if v is None:
        return "n/a"
    if isinstance(v, float):
        return f"{v:.{digits}f}"
    return str(v)


def fmt_stat(s):
    return "n/a" if not s else f"{s['p50']} / {s['p95']} (n={s['n']})"


def render_table(res):
    skipped = [l for l in res["levels"] if l.get("status") != "ok"]
    lv = {str(l["n"]) if not l.get("cold") else "cold": l for l in res["levels"] if l.get("status") == "ok"}
    cols = [k for k in lv]
    head = "| Metric | " + " | ".join(f"N={c}" if c != "cold" else "cold N=1" for c in cols) + " | Source |"
    sep = "|---|" + "---|" * len(cols) + "---|"
    src = f"`bench.sh {ENGINE}` {res['provenance']['utc']} (results.json levels)"
    rows = [
        ("Engine / fidelity", [res["provenance"]["fidelity"]] * len(cols), src),
        ("Total PSS, steady (MiB)", [fmt(lv[c].get("pss_steady_mib")) for c in cols], "PSS sum over agent trees"),
        ("Total private dirty, steady (MiB)", [fmt(lv[c].get("private_dirty_steady_mib")) for c in cols], "Private_Dirty sum"),
        ("Total PSS, peak during run (MiB)", [fmt(lv[c].get("pss_peak_mib")) for c in cols], "250 ms sampler max"),
        ("Host used delta, steady (MiB)", [fmt(lv[c].get("host_used_delta_steady_mib")) for c in cols], "MemTotal-MemAvailable Δ"),
        ("Incremental PSS per agent (MiB)", [fmt((lv[c].get("pss_steady_mib") - (res["core"] or {}).get("pss_mib", 0)) / (lv[c]["n"] or 1)) if lv[c].get("pss_steady_mib") is not None else "n/a" for c in cols],
         "(total − fixed core)/N"),
        ("Launch to first frame, RTT p50 / p95 (s-ms)", [fmt_stat(lv[c].get("launch_ms")) for c in cols], "per agent, concurrent"),
        ("Verified first frame (count 0)", [f"{lv[c]['first_frame_ok']}/{lv[c]['n']}" for c in cols], "tree read-back"),
        ("Fresh screenshot ms p50 / p95", [fmt_stat(lv[c].get("shot_ms")) for c in cols], "file written + checked"),
        ("UI tree ms p50 / p95", [fmt_stat(lv[c].get("tree_ms")) for c in cols], "tree with text"),
        ("Tap ack ms p50 / p95", [fmt_stat(lv[c].get("tap_ms")) for c in cols], "engine reply only"),
        ("Tap → state verified", [f"{lv[c]['tap_state_verified']}/{lv[c]['tap_total']}" for c in cols], "tree count k after k-th tap"),
        ("Tree after tap ms p50 / p95", [fmt_stat(lv[c].get("tap_tree_ms")) for c in cols], "separate from tap ack"),
        ("Reset ms p50 / p95", [fmt_stat(lv[c].get("reset_ms")) for c in cols],
         "rust-vm: VM drop + relaunch; art: fresh Activity" ),
        ("Reset verified (count 0)", [f"{lv[c]['reset_verified']}/{lv[c]['n']}" for c in cols], "tree read-back"),
        ("Verified tasks (all checks)", [f"{lv[c]['sessions_verified']}/{lv[c]['sessions_total']}" for c in cols], "per agent, every step"),
        ("Integral PSS (GiB·min)", [fmt(lv[c].get("integral_pss_gib_min"), 3) for c in cols], "trapezoid over samples"),
        ("Verified tasks / GiB-min", [fmt(lv[c].get("tasks_per_gib_min"), 2) for c in cols], "headline (PSS basis)"),
        ("Leftover processes after teardown", [str(lv[c].get("leftover_procs_after_teardown")) for c in cols], "must be 0"),
    ]
    out = [f"## {ENGINE} — art-runtime bench ({res['provenance']['utc']})", "",
           f"Workload: {res['workload']}", "", head, sep]
    for name, vals, s in rows:
        out.append(f"| {name} | " + " | ".join(vals) + f" | {s} |")
    if res["core"]:
        out.append("")
        out.append(f"Fixed core: {res['core'].get('pss_mib')} MiB PSS, {res['core'].get('private_dirty_mib', 'n/a')} MiB private dirty — {res['core'].get('note')}")
    o = res.get("oneshot") or {}
    if o:
        out.append("")
        out.append(f"One-shot row: `{o.get('cmd')}` wall p50/p95 = {fmt_stat(o.get('wall_ms'))}; verified {o.get('verified')}/{o.get('n')}")
    for u in res.get("unsupported", []):
        out.append(f"unsupported yet: {u['row']} — {u['reason']}")
    for s in skipped:
        out.append(f"skipped N={s['n']}: {s.get('status')}")
    return "\n".join(out) + "\n"


if __name__ == "__main__":
    main()
PY
"$ROOT/tools/guarded.sh" --mem "$MEM" --time "$TIME" -- \
    python3 -I "$OUT/driver.py" "$ENGINE" "$ROOT" "$OUT" "$NS" "$REPS" "$COLD" "$RAM_MAX" "$RAM_HARD" \
    "$TIME" "$APK" "$ACOS" "$ACOS_ART"
rc=$?
echo "bench.sh: $ENGINE done rc=$rc out=$OUT" >&2
exit "$rc"
