#!/usr/bin/env bash
#
# bench-v1.sh -- ACOS V1 benchmark: one command -> the ACOS-vs-legacy table.
#
#   tools/bench-v1.sh                       # release build, 5 rounds, writes results/bench/v1-<date>.json
#   tools/bench-v1.sh --rounds 3
#   ACOS_BIN=.cache/target-w100a/release/acos tools/bench-v1.sh
#
# What it measures (all at the same HEAD, one host window):
#   * cold process -> Vm ready, framework boot -> attachApplication, bindApplication
#   * launch before/after the ACOS framework snapshot (Vm::restore)
#   * SessionPool create / reset / throughput / marginal memory
#   * fixed idle memory (Vm-only and booted framework)
#   * per-session incremental + marginal memory (fresh process per N)
#   * interpreter microbenchmarks (VM counters)
#   * Bench* throughput vs `java -Xint` (5 interleaved rounds, min + median)
#   * hello-views renderer first frame / warm frame / PNG encode (fresh screenshot)
#   * `acos observe --tree`, `acos frame-diff`, `acos inspect`
#   * MCP tools/call round trip vs the bare CLI
#   * `acos serve` daemon round-trip reuse vs a fresh connection
#
# Method rules (BENCHMARKS.md §2): metric type and cold/warm are labelled; no
# fixed sleep is inside a timed path; memory types are never summed; the host
# load is recorded and noisy rows are flagged (spread > 15 % of the median).
# Fidelity is `native-runtime`; nothing here is ART-verified, and no legacy
# value is an ACOS result.
#
# Env:
#   ACOS_BIN   default: <root>/target/release/acos (build it first)
#   ACOS_MCP_BIN, VMBENCH_BIN, RENDER_BIN   optional prebuilt helpers
#   ACOS_BENCH_ROUNDS   default 5 (interleaved rounds)
#
# Host safety: every command is bounded by a wall timeout; the memory-heavy
# children are the same ones `tools/bench-acos.sh` runs under tools/guarded.sh.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ROUNDS="${ACOS_BENCH_ROUNDS:-5}"
STAMP="$(date +%Y-%m-%d-%H%M%S)"
OUT="${ACOS_BENCH_OUT:-$ROOT/results/bench/v1-$STAMP.json}"
ACOS="${ACOS_BIN:-$ROOT/target/release/acos}"
MCP="${ACOS_MCP_BIN:-$(dirname "$ACOS")/acos-mcp}"
VMBENCH="${VMBENCH_BIN:-$ROOT/target/release/examples/vmbench}"
RENDER="${RENDER_BIN:-$ROOT/target/release/examples/bench_render}"

while [ $# -gt 0 ]; do
    case "$1" in
        --rounds) ROUNDS="$2"; shift 2 ;;
        --out) OUT="$2"; shift 2 ;;
        --acos) ACOS="$2"; shift 2 ;;
        -h|--help) sed -n '2,30p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'; exit 0 ;;
        *) echo "bench-v1: unknown option $1" >&2; exit 2 ;;
    esac
done

for f in "$ACOS" "$MCP" "$VMBENCH" "$RENDER"; do
    if [ ! -x "$f" ]; then
        echo "bench-v1: missing executable $f" >&2
        echo "bench-v1: build with: cargo build --release -p acos-cli -p acos-mcp \\" >&2
        echo "bench-v1:   -p acos-vm --example vmbench -p acos-render --example bench_render" >&2
        exit 2
    fi
done

mkdir -p "$(dirname "$OUT")"

python3 - "$ROOT" "$ACOS" "$MCP" "$VMBENCH" "$RENDER" "$ROUNDS" "$OUT" <<'PY'
import datetime, json, os, re, shutil, socket, statistics, subprocess, sys, tempfile, time

ROOT, ACOS, MCP, VMBENCH, RENDER, ROUNDS, OUT = sys.argv[1:8]
ROUNDS = int(ROUNDS)
TMP = os.path.join(ROOT, "target", "tmp", "bench-v1")
shutil.rmtree(TMP, ignore_errors=True)
os.makedirs(TMP, exist_ok=True)
ENV = dict(os.environ)
ENV["ACOS_REQUIRE_FIXTURES"] = "1"

def run(cmd, timeout=900, env=None):
    return subprocess.run(cmd, check=False, stdout=subprocess.PIPE,
                          stderr=subprocess.PIPE, timeout=timeout,
                          env=env or ENV)

def now_ms():
    return time.monotonic() * 1000.0

def timed(cmd, timeout=900, env=None):
    t0 = now_ms()
    p = run(cmd, timeout=timeout, env=env)
    return now_ms() - t0, p

def stats(xs):
    xs = sorted(float(x) for x in xs)
    n = len(xs)
    p = lambda q: xs[min(n - 1, max(0, int(round(q * (n - 1)))))]
    med = statistics.median(xs)
    return {"n": n, "min": round(xs[0], 4), "median": round(med, 4),
            "p50": round(med, 4), "p95": round(p(0.95), 4),
            "max": round(xs[-1], 4),
            "spread_pct": round((xs[-1] - xs[0]) / med * 100.0, 2) if med else None,
            "raw": [round(x, 4) for x in xs]}

def bench(metric, extra=None, runs=None, timeout=900):
    runs = runs or ROUNDS
    cmd = [ACOS, "bench", metric, "--runs", str(runs), "--warmup", "1", "--json"]
    if extra:
        cmd += extra
    p = run(cmd, timeout=timeout)
    try:
        doc = json.loads(p.stdout.decode() or "{}")
    except Exception:
        doc = {}
    if not doc.get("ok"):
        sys.stderr.write(f"bench-v1: {metric} failed: {doc.get('error')}\n")
        return None, doc
    return doc["result"], doc

host = {
    "timestamp": datetime.datetime.now().isoformat(timespec="seconds"),
    "kernel": os.uname().release,
    "nproc": os.cpu_count(),
    "uptime": run(["uptime"]).stdout.decode().strip(),
    "loadavg": open("/proc/loadavg").read().strip(),
    "mem_total_kib": int(re.search(r"MemTotal:\s+(\d+)", open("/proc/meminfo").read()).group(1)),
    "mem_available_kib": int(re.search(r"MemAvailable:\s+(\d+)", open("/proc/meminfo").read()).group(1)),
    "commit": run(["git", "-C", ROOT, "rev-parse", "HEAD"]).stdout.decode().strip(),
    "acos_bin": ACOS,
    "profile": "release",
    "rounds": ROUNDS,
}

commit = host["commit"]
metrics = {}
raw = {}

# ---- 1. cold process -> Vm ready ------------------------------------------
r, raw["process-start"] = bench("process-start", runs=max(ROUNDS, 5))
if r: metrics["process_start"] = r["wall_ms"]

# ---- 2. framework boot -> attach ------------------------------------------
r, raw["boot-attach"] = bench("boot-attach", ["--apk", os.path.join(ROOT, "fixtures/build/apps/hello-views.apk")], runs=max(ROUNDS, 5))
if r:
    metrics["boot_attach"] = r["boot_ms"]
    metrics["boot_attach_cold_first"] = (r.get("first_vm_in_process") or {}).get("boot_ms")

# ---- 3. launch before/after snapshot --------------------------------------
r, raw["launch"] = bench("launch", ["--apk", os.path.join(ROOT, "fixtures/build/apps/hello-views.apk")], runs=max(ROUNDS, 5))
if r:
    metrics["launch_before"] = r["before"]["launch_ms"]
    metrics["launch_before_boot"] = r["before"]["boot_ms"]
    metrics["launch_restore"] = r["after"]["restore_ms"]
    metrics["launch_after_total"] = r["after"]["total_ms"]
    metrics["launch_after_launch"] = r["after"]["launch_ms"]
    metrics["launch_template_boot_ms"] = {"min": r["template"]["boot_ms"], "median": r["template"]["boot_ms"], "raw": [r["template"]["boot_ms"]]}
    metrics["launch_snapshot_bytes"] = r["template"]["snapshot_bytes"]
    metrics["launch_app_created"] = r["before"]["app_created"]
    metrics["launch_tx_delivered"] = r["before"]["transaction_delivered"]
    metrics["launch_blocker"] = r["before"]["first_blocker"]

# ---- 4. SessionPool create / reset / memory ------------------------------
pool_dex = os.path.join(ROOT, "fixtures/build/dexprogs/02_int_arith/classes.dex")
r, raw["sessions-pool"] = bench("sessions-pool", ["--sessions", "1,2,5,10,20", "--dex", pool_dex], runs=max(ROUNDS, 3), timeout=1800)
pool = {}
if r:
    for s in r["sessions"]:
        n = s["n"]
        sc = s.get("session_create", {})
        pool[str(n)] = {
            "create_ms": round(sc.get("create_nanos_p50_per_session", {}).get("p50", 0) / 1e6, 4) if sc else None,
            "create_per_sec": sc.get("sessions_per_sec", {}).get("p50"),
            "reset_ms": s.get("reset_ms_p50"),
            "jobs_per_sec": (s.get("throughput") or {}).get("jobs_per_sec", {}).get("p50") if s.get("throughput") else None,
            "total_mib": s.get("miB_anon_plus_shmem_p50"),
        }
    metrics["pool_sessions"] = pool
    if r.get("marginal"):
        m = r["marginal"][-1]
        metrics["pool_marginal_mib"] = {"min": m.get("marginal_miB_per_session"), "median": m.get("marginal_miB_per_session"), "raw": [m.get("marginal_miB_per_session")]}

# ---- 5. fixed idle memory --------------------------------------------------
r, raw["idle-rss"] = bench("idle-rss", ["--apk", os.path.join(ROOT, "fixtures/build/apps/hello-views.apk")], runs=max(ROUNDS, 5), timeout=1800)
if r:
    v = r["variants"]
    metrics["idle_booted_mib"] = {"min": v["booted"]["miB_anon_plus_shmem_p50"], "median": v["booted"]["miB_anon_plus_shmem_p50"], "raw": [v["booted"]["miB_anon_plus_shmem_p50"]]}
    metrics["idle_vm_mib"] = {"min": v["vm"]["miB_anon_plus_shmem_p50"], "median": v["vm"]["miB_anon_plus_shmem_p50"], "raw": [v["vm"]["miB_anon_plus_shmem_p50"]]}
    metrics["idle_booted_pss_mib"] = {"min": v["booted"]["miB_pss_p50"], "median": v["booted"]["miB_pss_p50"], "raw": [v["booted"]["miB_pss_p50"]]}

# ---- 6. per-session incremental + marginal memory -------------------------
r, raw["sessions"] = bench("sessions", ["--sessions", "1,2,5,10", "--apk", os.path.join(ROOT, "fixtures/build/apps/hello-views.apk")], runs=max(ROUNDS, 5), timeout=1800)
if r:
    inc = {str(x["n"]): x["incremental_miB_per_session"] for x in r["incremental"]}
    metrics["sessions_incremental_mib"] = inc
    metrics["sessions_marginal_mib"] = {f'{m["from_n"]}->{m["from_n"]+1}': m["marginal_miB_per_session"] for m in r.get("marginal", [])}

# ---- 7. interpreter microbenchmarks --------------------------------------
r, raw["interpreter"] = bench("interpreter", runs=max(ROUNDS, 5), timeout=1800)
if r:
    metrics["interpreter"] = [{"program": x["program"], "wall_ms": x["wall_ms"], "instructions": x["instructions"]} for x in r["programs"]]

# ---- 8. Bench* vs java -Xint (interleaved) --------------------------------
java = shutil.which("java") or "java"
sdk = os.environ.get("ANDROID_HOME", "/home/Qui/Android/Sdk")
build_tools = os.environ.get("BUILD_TOOLS_VERSION", "35.0.0")
d8 = os.path.join(sdk, "build-tools", build_tools, "d8")
android_jar = os.path.join(sdk, "platforms", "android-34", "android.jar")
programs = ["BenchArith", "BenchLoop", "BenchArray", "BenchFields", "BenchFib",
            "BenchStaticCall", "BenchVirtualCall", "BenchVirtual", "BenchAlloc",
            "BenchGc", "BenchString", "BenchCollections"]
vm_rows = {}
for name in programs:
    src = os.path.join(ROOT, "crates/acos-vm/benches/java", name + ".java")
    outdir = os.path.join(TMP, name)
    classes = os.path.join(outdir, "classes")
    dex = os.path.join(outdir, "classes.dex")
    if not os.path.exists(dex):
        os.makedirs(classes, exist_ok=True)
        p = run(["javac", "--release", "11", "-d", classes, src])
        if p.returncode != 0:
            sys.stderr.write(f"bench-v1: javac failed for {name}\n")
            continue
        cfs = sorted(os.path.join(classes, f) for f in os.listdir(classes) if f.endswith(".class"))
        p = run([d8, "--lib", android_jar, "--min-api", "26", "--output", outdir] + cfs)
        if p.returncode != 0:
            sys.stderr.write(f"bench-v1: d8 failed for {name}\n")
            continue
    acos_ms, java_ms = [], []
    for _ in range(ROUNDS):
        dt, p = timed([VMBENCH, dex, name, "1"], timeout=300)
        if p.returncode == 0:
            acos_ms.append(dt)
        dt, p = timed([java, "-Xint", "-cp", classes, name], timeout=300)
        if p.returncode == 0:
            java_ms.append(dt)
    if acos_ms and java_ms:
        vm_rows[name] = {"acos": stats(acos_ms), "java_xint": stats(java_ms)}

# ---- 9. renderer first frame + fresh screenshot --------------------------
render_path = os.path.join(TMP, "render.json")
p = run([RENDER, render_path], timeout=600)
render = {}
if p.returncode == 0 and os.path.exists(render_path):
    try:
        render = json.load(open(render_path))
    except Exception:
        render = {}
metrics["render_hello_cold_ms"] = {"min": render.get("helloColdMs"), "median": render.get("helloColdMs"), "raw": [render.get("helloColdMs")]}
metrics["render_hello_warm_ms"] = render.get("helloWarmMs")
metrics["render_capture_ms"] = render.get("helloCaptureMs")
enc = (render.get("encoding") or {}).get("FastFiltered", {}).get("ms", {})
metrics["render_png_fastfiltered_ms"] = enc
if render.get("helloWarmMs") and enc:
    w = render["helloWarmMs"].get("p50", 0) or 0
    g = enc.get("p50", 0) or 0
    metrics["screenshot_warm_total_ms"] = {"min": w + g, "median": w + g,
                                           "raw": [w + g], "type": "renderer warm raster + PNG FastFiltered"}

# ---- 10. observe-tree / frame-diff / apk inspect ------------------------
def loop(cmd, n=ROUNDS, timeout=300, env=None):
    # Latency is timed regardless of exit status: `observe --tree` is expected
    # to answer "not_ready" until the UI-tree bridge lands, and timing that
    # answer is the honest measurement.
    xs = []
    for _ in range(n):
        dt, _p = timed(cmd, timeout=timeout, env=env)
        xs.append(dt)
    return stats(xs) if xs else None

metrics["observe_tree_ms"] = loop([ACOS, "observe", "--tree", "--json"])
ref_png = os.path.join(ROOT, "fixtures/reference/hello-views/art-api34-x86_64/screenshot_initial.png")
cand_png = os.path.join(ROOT, "results/render/hello-views.png")
if os.path.exists(ref_png) and os.path.exists(cand_png):
    metrics["frame_diff_ms"] = loop([ACOS, "frame-diff", ref_png, cand_png, "--json"])
    p = run([ACOS, "frame-diff", ref_png, cand_png, "--json"])
    try:
        metrics["frame_diff_report"] = json.loads(p.stdout.decode()).get("result")
    except Exception:
        metrics["frame_diff_report"] = None
metrics["apk_inspect_ms"] = loop([ACOS, "inspect", os.path.join(ROOT, "fixtures/build/apps/hello-views.apk"), "--json"])

# ---- 11. MCP round trip vs bare CLI --------------------------------------
mcp_script = os.path.join(ROOT, "crates/acos-mcp/tests/bench-overhead.sh")
mcp = {}
if os.path.exists(mcp_script):
    p = run(["bash", mcp_script, str(max(ROUNDS, 10))], timeout=600,
            env={**ENV, "ACOS_BIN": ACOS, "ACOS_MCP_BIN": MCP, "ACOS_ALLOWED_ROOTS": ROOT})
    for line in p.stdout.decode().splitlines():
        if "=" in line:
            k, v = line.split("=", 1)
            mcp[k] = v
    metrics["mcp"] = mcp

# ---- 12. daemon session reuse -------------------------------------------
daemon = {}
sock_path = os.path.join(TMP, "session.sock")
serve = subprocess.Popen([ACOS, "serve", "--socket", sock_path, "--idle-timeout", "120"],
                         stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, env=ENV)
try:
    for _ in range(100):
        if os.path.exists(sock_path):
            break
        time.sleep(0.05)
    if os.path.exists(sock_path):
        def rpc(conn, i):
            req = json.dumps({"id": i, "tool": "ping", "args": {}}) + "\n"
            t0 = now_ms()
            conn.sendall(req.encode())
            buf = b""
            while not buf.endswith(b"\n"):
                chunk = conn.recv(65536)
                if not chunk:
                    break
                buf += chunk
            return now_ms() - t0
        reuse = []
        c = socket.socket(socket.AF_UNIX)
        c.settimeout(20)
        c.connect(sock_path)
        for i in range(20):
            reuse.append(rpc(c, i))
        c.close()
        fresh = []
        for i in range(10):
            c = socket.socket(socket.AF_UNIX)
            c.settimeout(20)
            c.connect(sock_path)
            fresh.append(rpc(c, i))
            c.close()
        daemon["reuse_ms"] = stats(reuse)
        daemon["fresh_conn_ms"] = stats(fresh)
        # process-spawn path: `acos session list` through the running daemon
        spawn = []
        for _ in range(ROUNDS):
            dt, p = timed([ACOS, "session", "list", "--json"], timeout=120,
                          env={**ENV, "ACOS_SESSION_SOCKET": sock_path})
            if p.returncode == 0:
                spawn.append(dt)
        daemon["cli_process_ms"] = stats(spawn) if spawn else None
finally:
    serve.terminate()
    try:
        serve.wait(timeout=5)
    except Exception:
        serve.kill()

metrics["daemon"] = daemon

# ---- 13. framework-bound capabilities at this HEAD -----------------------
# These paths did not exist when §8 was written: a launched Activity reaching
# first_frame (M3), a live View-tree dump (M7), a completed-frame retrieval
# (M5) and an input action (M8). `ACOS_NO_BOOT_CACHE` is used for the
# observe/tap rows because the default boot-image *restore* path is broken at
# this commit (see launch_default) — an open bug, never a hidden pass.
counter = os.path.join(ROOT, "fixtures/build/apps/counter.apk")
NOCACHE = {"ACOS_NO_BOOT_CACHE": "1"}

def run_json(cmd, env_extra=None, timeout=300):
    e = dict(ENV)
    if env_extra:
        e.update(env_extra)
    t0 = now_ms()
    p = run(cmd, timeout=timeout, env=e)
    return now_ms() - t0, p

def doc_of(p):
    try:
        return json.loads(p.stdout.decode() or "{}")
    except Exception:
        return {}

def launch_sample(env_extra=None):
    dt, p = run_json([ACOS, "launch", counter, "--json"], env_extra)
    d = doc_of(p); r = d.get("result") or {}
    return {"outer_ms": round(dt, 2), "status": d.get("status"),
            "stage": r.get("reached_stage"), "wall_ms": r.get("wall_ms"),
            "peak_rss_mib": round((r.get("peak_rss_bytes") or 0) / 2**20, 1)}

def cli_sample(args, env_extra=NOCACHE):
    dt, p = run_json([ACOS] + args, env_extra)
    d = doc_of(p); r = d.get("result") or {}
    return {"ms": round(dt, 2), "status": d.get("status"),
            "stage": r.get("stage_reached"), "accepted": r.get("accepted"),
            "state_changed": r.get("state_changed")}

fw_n = max(ROUNDS, 5)
fw = {"n": fw_n}
fw["launch_default"] = [launch_sample() for _ in range(fw_n)]
fw["launch_nocache"] = [launch_sample(NOCACHE) for _ in range(fw_n)]
shot_png = os.path.join(TMP, "counter-frame.png")
fw["observe_tree_apk"] = [cli_sample(["observe", "--apk", counter, "--tree", "--json"]) for _ in range(fw_n)]
fw["observe_screenshot_apk"] = [cli_sample(["observe", "--apk", counter, "--screenshot", shot_png, "--json"]) for _ in range(fw_n)]
fw["tap_apk"] = [cli_sample(["tap", "--apk", counter, "--text", "Increment", "--json"]) for _ in range(fw_n)]

# Pure in-session latency (no relaunch per call): own daemon, private socket.
dsock = os.path.join(TMP, "fw.sock")
try:
    os.remove(dsock)
except OSError:
    pass
denv = {**ENV, "ACOS_NO_BOOT_CACHE": "1", "ACOS_SESSION_SOCKET": dsock}
dserve = subprocess.Popen([ACOS, "serve", "--socket", dsock, "--idle-timeout", "120"],
                          stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, env=denv)
dm = {}
try:
    for _ in range(100):
        if os.path.exists(dsock):
            break
        time.sleep(0.05)
    if os.path.exists(dsock):
        senv = {"ACOS_NO_BOOT_CACHE": "1", "ACOS_SESSION_SOCKET": dsock}
        dt, p = run_json([ACOS, "launch", counter, "--keep", "--json"], senv)
        d = doc_of(p); r = d.get("result") or {}
        dm["launch"] = {"ms": round(dt, 2), "status": d.get("status"), "stage": r.get("reached_stage")}
        def sess(args):
            xs = []; last = {}
            for _ in range(max(ROUNDS, 5)):
                dt, p = run_json([ACOS] + args + ["--session", "s_cli", "--json"], senv)
                xs.append(round(dt, 2)); last = doc_of(p)
            r = last.get("result") or {}
            return stats(xs), {"status": last.get("status"), "stage": r.get("stage_reached"),
                               "accepted": r.get("accepted"), "state_changed": r.get("state_changed")}
        dm["tree"], dm["tree_info"] = sess(["observe", "--tree"])
        dm["screenshot"], dm["screenshot_info"] = sess(["observe", "--screenshot", os.path.join(TMP, "sess-frame.png")])
        dm["tap"], dm["tap_info"] = sess(["tap", "--text", "Increment"])
finally:
    dserve.terminate()
    try:
        dserve.wait(timeout=5)
    except Exception:
        dserve.kill()
    try:
        os.remove(dsock)
    except OSError:
        pass
fw["daemon"] = dm
metrics["fw"] = fw

# ---- legacy table (BENCHMARKS.md §3/§5; reference only) ------------------
LEGACY = {
    "M1": ("Fixed idle core memory", 206.0, "MiB anon+shmem (W1) / 303.9 (v9)"),
    "M2": ("Per-session memory at N=5", 78.7, "MiB/session anon+shmem (v9); 33.2 W2 opt-in"),
    "M3": ("Cold start -> first correct frame", 3.620, "s, cold (v12)"),
    "M4": ("Worker create -> correct frame", 1.297, "s p50 (1.251-1.343)"),
    "M5": ("Fresh screenshot", 0.0665, "s p50 (0.066-0.067)"),
    "M6": ("Reset-to-ready", 0.1103, "s cmd p50 / 0.7481 s -> frame"),
    "M7": ("UI tree with text", 0.538, "s one-shot 538-768; persistent 4.1 ms; structure 14-87 ms"),
    "M8": ("Action-to-state latency", None, "not cleanly published"),
    "M9": ("Concurrent workloads", None, "N=5 scorecard, N=10 measured"),
    "M10": ("End-to-end verify wall", 19.0, "s (install-bound dev loop)"),
    "M11": ("Verified tasks / GiB-minute", None, "not published"),
}

def med(k):
    v = metrics.get(k)
    if isinstance(v, dict):
        for key in ("median", "p50"):
            if v.get(key) is not None:
                return v[key]
        return None
    return v

rows = []
def add(mid, name, acos_v, legacy_v, legacy_label, ratio, verdict, plan, note=""):
    rows.append({"id": mid, "metric": name, "acos": acos_v, "legacy": legacy_v,
                 "legacy_label": legacy_label, "ratio": ratio, "verdict": verdict,
                 "plan": plan, "note": note})

def _ms_stats(rows_, field="ms"):
    xs = [r.get(field) for r in rows_ if r.get(field) is not None]
    return stats(xs) if xs else None

def _ff(rows_):
    return sum(1 for r in rows_ if r.get("stage") == "first_frame")

fw = metrics.get("fw") or {}
fw_nc = fw.get("launch_nocache") or []
fw_def = fw.get("launch_default") or []
nc = _ms_stats(fw_nc, "outer_ms")
def_st = _ms_stats(fw_def, "outer_ms")
m3 = round(nc["median"] / 1000.0, 4) if nc else None
m3_ok = bool(nc) and _ff(fw_nc) == len(fw_nc)
dm = fw.get("daemon") or {}
tree_stats = dm.get("tree")
shot_stats = dm.get("screenshot")
tap_stats = dm.get("tap")
tree_info = dm.get("tree_info") or {}
shot_info = dm.get("screenshot_info") or {}
tap_info = dm.get("tap_info") or {}

def _fmt_stats(s, field="median"):
    return s[field] if s else None

idle = med("idle_booted_mib")
inc5 = (metrics.get("sessions_incremental_mib") or {}).get("5")
add("M1", LEGACY["M1"][0], idle, LEGACY["M1"][1], LEGACY["M1"][2],
    round(idle / LEGACY["M1"][1], 3) if idle else None,
    "PASS" if idle and idle <= LEGACY["M1"][1] else "FAIL",
    "Reduce booted core RSS; keep the BootCache/codecache sharing." if idle and idle > LEGACY["M1"][1] else "",
    "ACOS booted framework anon+shmem vs legacy W1; same type, different engine state (disclosed in §3).")
add("M2", LEGACY["M2"][0], inc5, LEGACY["M2"][1], LEGACY["M2"][2],
    round(inc5 / LEGACY["M2"][1], 3) if inc5 else None,
    "PASS" if inc5 and inc5 <= LEGACY["M2"][1] else "FAIL",
    "Finish the zygote-like template so the first session stops paying the boot classpath (MEMORY_PROFILE.md §5)." if inc5 and inc5 > LEGACY["M2"][1] else "",
    "N=5 incremental, anon+shmem.")
cold_frame = med("render_hello_cold_ms")
add("M3", LEGACY["M3"][0], m3, LEGACY["M3"][1], LEGACY["M3"][2],
    round(m3 / LEGACY["M3"][1], 4) if m3 else None,
    "PASS" if (m3_ok and m3 and m3 <= LEGACY["M3"][1]) else ("FAIL" if m3 else "NOT_MEASURED"),
    "Fix the ACOSSNAP restore path so the default warm launch reaches first_frame (it fails at activity_on_create 9/10); then score the launched frame against the ART reference.",
    f"Framework-bound `acos launch counter.apk` -> first_frame, boot cache OFF (cold): {_ff(fw_nc)}/{len(fw_nc)} reached, p50 {_fmt_stats(nc)} ms outer. Default cache ON: {_ff(fw_def)}/{len(fw_def)} reached, p50 {_fmt_stats(def_st)} ms. RSS p50 {_fmt_stats(_ms_stats(fw_nc, 'peak_rss_mib'))} MiB. Launched frame correctness not ART-scored.")
pool_create = (pool.get("5") or {}).get("create_ms")
add("M4", LEGACY["M4"][0], round(pool_create / 1000.0, 6) if pool_create else None, LEGACY["M4"][1], LEGACY["M4"][2],
    None, "FAIL", "Add a worker-create path that reaches a frame; SessionPool restore is not worker-create->frame.",
    "SessionPool restore is a quiescent core VM, no frame.")
shot = med("screenshot_warm_total_ms")
shot_ms = _fmt_stats(shot_stats)
add("M5", LEGACY["M5"][0], round(shot_ms / 1000.0, 6) if shot_ms else None, LEGACY["M5"][1], LEGACY["M5"][2],
    round((shot_ms / 1000.0) / LEGACY["M5"][1], 4) if shot_ms else None,
    "PASS" if (shot_ms and shot_ms / 1000.0 <= LEGACY["M5"][1] and shot_info.get("status") == "pass")
            else ("FAIL" if shot_ms else "NOT_MEASURED"),
    "Route `observe --screenshot` through the daemon session (same-process `observe --apk` already publishes a CompletedFrame) and keep the frame request-sequenced.",
    f"Daemon-session completed-frame retrieval p50 {shot_ms} ms (status={shot_info.get('status')}, stage={shot_info.get('stage')}); process stdout is wall-timed, includes PNG encode+write. Self-contained `observe --apk --screenshot` p50 {_fmt_stats(_ms_stats(fw.get('observe_screenshot_apk') or []))} ms incl. launch. Renderer-only warm encode is {shot} ms.")
reset = (pool.get("5") or {}).get("reset_ms")
add("M6", LEGACY["M6"][0], reset, LEGACY["M6"][1] * 1000.0, LEGACY["M6"][2],
    round(reset / (LEGACY["M6"][1] * 1000.0), 4) if reset else None,
    "FAIL", "Implement app-scoped reset -> first frame; pool reset is a core-VM restore with no app/frame.",
    "Pool reset-to-clean (N=5); legacy is app reset + frame.")
tree_ms = _fmt_stats(tree_stats)
add("M7", LEGACY["M7"][0], tree_ms, LEGACY["M7"][1] * 1000.0, LEGACY["M7"][2],
    round(tree_ms / (LEGACY["M7"][1] * 1000.0), 4) if tree_ms else None,
    "PASS" if (tree_ms and tree_ms <= LEGACY["M7"][1] * 1000.0 and tree_info.get("status") == "pass")
            else ("FAIL" if tree_ms else "NOT_MEASURED"),
    "Keep the tree bridge; add text-equality checks against `ui_initial.xml` (currently only the shape is checked).",
    f"Live View-tree dump from a kept daemon session (counter, stage={tree_info.get('stage')}); one dump with text nodes. Legacy one-shot 538-768 ms, structure 14-87 ms, persistent 4.1 ms; ACOS {tree_ms} ms. `observe --tree` without a session is still the not_ready path ({med('observe_tree_ms')} ms).")
tap_ms = _fmt_stats(tap_stats)
add("M8", "Action-to-state latency", round(tap_ms / 1000.0, 6) if tap_ms else None, None, LEGACY["M8"][2],
    None, "FAIL",
    "Land the tap: the input is acked but `state_changed=false` and the count stays `0` (W144-A leash fix pending).",
    f"`acos tap --text Increment` accepted={tap_info.get('accepted')} state_changed={tap_info.get('state_changed')} p50 {tap_ms} ms; no observed state change.")
add("M9", "Concurrent workloads", (metrics.get("pool_sessions") or {}).get("20", {}).get("total_mib"), None, LEGACY["M9"][2], None, "FAIL",
    "Per-session Binder transports so N sessions are independently usable, then assert independent state/input at N.", "memory at N=20 only; independent state not proven")
add("M10", "End-to-end verify wall", None, LEGACY["M10"][1], LEGACY["M10"][2], None, "FAIL",
    "Close launch/assert/evidence/reset/cleanup so `acos verify` passes on the fixtures.", "verify stops at the first UI step")
add("M11", "Verified tasks / GiB-minute", None, None, LEGACY["M11"][2], None, "FAIL",
    "Run the T01-T12 suite with job-cgroup accounting.", "not published")
add("L-launch", "Launch: cold process -> Vm ready", med("process_start"), None, "no legacy metric", None, "N/A",
    "", "ACOS-native; no legacy baseline")
add("L-boot", "Launch: framework boot -> attach", med("boot_attach"), None, "no legacy metric", None, "N/A", "", "ACOS-native")
add("L-restore", "Launch: snapshot restore (Vm::restore)", med("launch_restore"), None, "no legacy metric", None, "N/A", "",
    "ACOSSNAP; before arm = Vm::new + boot_app_process")
add("L-before", "Launch: before snapshot (Vm::new + boot)", med("launch_before"), None, "no legacy metric", None, "N/A", "", "")
add("L-after", "Launch: after snapshot (restore + launch)", med("launch_after_total"), None, "no legacy metric", None, "N/A", "", "")
add("P-create", "SessionPool create (N=5)", pool_create, None, "no legacy metric", None, "N/A", "", "Vm::restore per session")
add("P-reset", "SessionPool reset (N=5)", reset, LEGACY["M6"][1] * 1000.0, "legacy app reset cmd 110.3 ms (different scope)", None, "N/A", "",
    "core VM restore, no app/frame")
add("R-cold", "Renderer cold first frame (hello-views)", cold_frame, None, "legacy 3.620 s is a full engine cold start", None, "N/A", "", "renderer standalone")
add("R-shot", "Fresh screenshot encode (warm raster + PNG)", shot, LEGACY["M5"][1] * 1000.0, "legacy 66.5 ms whole-session", round(shot / (LEGACY["M5"][1] * 1000.0), 4) if shot else None, "N/A", "", "renderer standalone")
add("X-diff", "frame-diff (1080x2400)", med("frame_diff_ms"), None, "no legacy metric", None, "N/A", "", "per-pixel + SSIM")
add("X-inspect", "APK parse / inspect", med("apk_inspect_ms"), None, "no legacy metric", None, "N/A", "", "static analysis")
add("X-mcp", "MCP tools/call round trip", float(mcp.get("mcp_p50_ms", 0)) if mcp.get("mcp_p50_ms") else None,
    float(mcp.get("bare_p50_ms", 0)) if mcp.get("bare_p50_ms") else None, "bare CLI p50", None, "N/A", "", "server overhead = mcp - bare")
add("X-daemon", "Daemon session reuse (ping)", (daemon.get("reuse_ms") or {}).get("median"),
    (daemon.get("cli_process_ms") or {}).get("median"), "CLI process-spawn p50", None, "N/A", "", "persistent unix-socket connection vs process spawn")

vm_summary = {}
for name, row in vm_rows.items():
    a, j = row["acos"], row["java_xint"]
    vm_summary[name] = {"acos_min": a["min"], "acos_median": a["median"],
                        "java_min": j["min"], "java_median": j["median"],
                        "acos_over_java": round(a["median"] / j["median"], 3) if j["median"] else None}

doc = {
    "schema": "acos-bench-v1/v1",
    "commit": commit,
    "host": host,
    "fidelity": "native-runtime (not ART-verified)",
    "method": {
        "rounds": ROUNDS,
        "min_and_median": True,
        "interleaved_vm": "vmbench | java -Xint per round, same window",
        "memory_type": "anon+shmem (RssAnon+RssShmem) unless a row says PSS; never summed with PSS",
        "timed_paths": "no fixed sleeps; each command is wall-timed around process start->exit",
        "noise_flag": "spread_pct > 15 % of the median (other agents run concurrently)",
        "guarded": "same child family as tools/bench-acos.sh (tools/guarded.sh bounds); this wrapper uses wall timeouts",
    },
    "metrics": metrics,
    "rows": rows,
    "vm_throughput": vm_summary,
    "raw_envelopes": raw,
}
os.makedirs(os.path.dirname(OUT), exist_ok=True)
with open(OUT, "w") as f:
    json.dump(doc, f, indent=2)

# ---- print the table ------------------------------------------------------
print(f"\nACOS V1 benchmark  commit {commit[:12]}  rounds={ROUNDS}  profile=release")
print(f"host: {host['loadavg']}  mem_available={host['mem_available_kib']//1024} MiB / {host['mem_total_kib']//1024} MiB")
print(f"out:  {OUT}\n")
print(f"{'#':<11} {'metric':<40} {'ACOS':>12} {'legacy':>12} {'ratio':>7}  verdict")
print("-" * 100)
for r in rows:
    a = "—" if r["acos"] is None else (f"{r['acos']:.4g}" if isinstance(r["acos"], (int, float)) else str(r["acos"]))
    l = "—" if r["legacy"] is None else (f"{r['legacy']:.4g}" if isinstance(r["legacy"], (int, float)) else str(r["legacy"]))
    ratio = "—" if r["ratio"] is None else f"{r['ratio']:.3f}"
    print(f"{r['id']:<11} {r['metric'][:40]:<40} {a:>12} {l:>12} {ratio:>7}  {r['verdict']}")
print("\nVM throughput (median ms; ACOS / java -Xint):")
for name, row in sorted(vm_summary.items()):
    print(f"  {name:<20} acos={row['acos_median']:>10.2f}  java={row['java_median']:>10.2f}  x={row['acos_over_java']:>6.3f}")
noisy = [k for k, v in metrics.items() if isinstance(v, dict) and (v.get("spread_pct") or 0) > 15]
if noisy:
    print("\nnoisy rows (spread > 15%): " + ", ".join(noisy))
print("\nnote: every row is renderer/runtime-measured here; no legacy value is an ACOS result.")
PY

echo
echo "bench-v1: wrote $OUT"
