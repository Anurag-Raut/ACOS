#!/usr/bin/env bash
#
# load-repro.sh -- W43: reproduce and classify the *harness* failures that
# `bench-d2` M9 (concurrent `acos verify --engine art` cohorts) shows under host
# load, with the FIRST error of every failed session captured.
#
#   tools/art-runtime/load-repro.sh [--n 20] [--reps 5] [--spinners 24]
#                                   [--acos PATH] [--art PATH] [--apk PATH]
#                                   [--scenario PATH] [--out DIR] [--timeout 600]
#
# Every rep spawns N sessions at once (like bench-d2's cohort child: ONE shared
# `ACOS_ART_DATA` per rep, one `--artifacts` dir per session) and records for
# each session:
#   * rc of `acos verify`, the parsed error code/message (the CLI's first error),
#   * the exit status of the `acos-art launch --interactive` session itself
#     (a wrapper records it: 137 = SIGKILL, 101 = Rust panic, ...),
#   * the guest's own stdout/stderr (ACOSLAUNCH_GUEST_{STDOUT,STDERR}_FILE),
#   * the guest cgroup: memory.current/peak, memory.events (oom, oom_kill, max),
#     pids.current/pids.max, memory.max, and the last values before it vanished,
#   * loadavg / MemAvailable at start and end.
#
# It does NOT build and it does NOT take the host lock: call it inside
#   tools/art-runtime/run-exclusive.sh --task W43 -- \
#     tools/guard/guarded.sh --mem 16G --time 1200 -- tools/art-runtime/load-repro.sh ...
# The CPU load is bounded (spinner processes, no allocations); RAM stays low.
#
# Output: <out>/run.json (all sessions), <out>/rep-<k>/s<i>/ (stdout.json,
# stderr.txt, guest-*.stdout, guest-*.stderr, session-*.log) and a
# one-screen summary on stderr.
set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT" || exit 1

N=20
REPS=5
SPINNERS=24
ACOS="$ROOT/.cache/w39-target/release/acos"
ART="$ROOT/.cache/w39-target/release/acos-art"
APK="$ROOT/fixtures/build/apps/counter.apk"
SCEN="$ROOT/scenarios/v2-art-counter.yaml"
OUT=""
TIMEOUT=600

while [ $# -gt 0 ]; do
    case "$1" in
        --n)        N="$2"; shift 2 ;;
        --reps)     REPS="$2"; shift 2 ;;
        --spinners) SPINNERS="$2"; shift 2 ;;
        --acos)     ACOS="$2"; shift 2 ;;
        --art)      ART="$2"; shift 2 ;;
        --apk)      APK="$2"; shift 2 ;;
        --scenario) SCEN="$2"; shift 2 ;;
        --out)      OUT="$2"; shift 2 ;;
        --timeout)  TIMEOUT="$2"; shift 2 ;;
        -h|--help)  sed -n '2,30p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'; exit 0 ;;
        *) echo "load-repro: unknown argument: $1" >&2; exit 2 ;;
    esac
done
[ -n "$OUT" ] || OUT="$ROOT/.cache/w43-repro/$(date -u +%Y%m%dT%H%M%SZ)"

for f in "$ACOS" "$ART" "$APK" "$SCEN"; do
    [ -e "$f" ] || { echo "load-repro: missing $f" >&2; exit 3; }
done
mkdir -p "$OUT"

cat > "$OUT/driver.py" <<'PY'
import json, os, re, shutil, signal, subprocess, sys, threading, time

cfg = json.load(open(sys.argv[1]))
ROOT, ACOS, ART, APK, SCEN = cfg["root"], cfg["acos"], cfg["art"], cfg["apk"], cfg["scenario"]
N, REPS, SPINNERS, OUT, TIMEOUT = cfg["n"], cfg["reps"], cfg["spinners"], cfg["out"], cfg["timeout"]
MiB = 1024.0 * 1024.0

def read(p):
    try:
        return open(p).read()
    except OSError:
        return ""

def meminfo():
    out = {}
    for line in read("/proc/meminfo").splitlines():
        k, _, v = line.partition(":")
        v = v.strip().split()[0] if v.strip() else "0"
        try:
            out[k] = int(v) / 1024.0
        except ValueError:
            out[k] = 0.0
    return {"mem_total_mib": round(out.get("MemTotal", 0), 1),
            "mem_available_mib": round(out.get("MemAvailable", 0), 1)}

def loadavg():
    f = read("/proc/loadavg").split()
    return {"load1": float(f[0]), "load5": float(f[1]), "load15": float(f[2])} if len(f) >= 3 else {}

# ---- wrapper: records the *session* exit status, keeps the full stderr ------
WRAP = os.path.join(OUT, "art-wrapper.sh")
with open(WRAP, "w") as f:
    f.write(r'''#!/usr/bin/env bash
real="$W43_REAL_ART"
sdir="$W43_SESSION_DIR"
"$real" "$@" 2> >(tee -a "$sdir/session-stderr.log" >&2)
rc=$?
printf 'session_rc=%s\n' "$rc" >> "$sdir/session-exit.txt"
printf 'session_pid=%s\n' "$$" >> "$sdir/session-exit.txt"
exit $rc
''')
os.chmod(WRAP, 0o755)

# ---- bounded CPU load ------------------------------------------------------
spinners = []
for _ in range(SPINNERS):
    spinners.append(subprocess.Popen(["/bin/sh", "-c", "while :; do :; done"],
                                     stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL))
time.sleep(1.0)

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
        return None

def cg_events(rel, name):
    out = {}
    for line in read(os.path.join("/sys/fs/cgroup", rel.lstrip("/"), name)).splitlines():
        k, _, v = line.partition(" ")
        try:
            out[k] = int(v.strip())
        except ValueError:
            pass
    return out

def comm(pid):
    return read("/proc/%d/comm" % pid).strip()

def sample_session(s, pm):
    guest = None
    for pid in tree([s["p"].pid], pm):
        if comm(pid) == "dalvikvm64":
            guest = pid
            break
    if guest is None:
        return
    rel = cgpath(guest)
    if not rel:
        return
    ev = cg_events(rel, "memory.events")
    rec = {"t": time.time(), "cgroup": rel,
           "current": cg_read(rel, "memory.current"),
           "peak": cg_read(rel, "memory.peak"),
           "pids_current": cg_read(rel, "pids.current"),
           "pids_max": cg_read(rel, "pids.max"),
           "memory_max": cg_read(rel, "memory.max"),
           "oom": ev.get("oom"), "oom_kill": ev.get("oom_kill"), "max": ev.get("max")}
    s["samples"].append(rec)

reps = []
for rep in range(REPS):
    rdir = os.path.join(OUT, "rep-%d" % rep)
    shutil.rmtree(rdir, ignore_errors=True)
    os.makedirs(rdir)
    data = os.path.join(rdir, "data")
    os.makedirs(data)
    host_before = {**meminfo(), **loadavg(),
                   "disk_free_gib": shutil.disk_usage(OUT).free / (1024.0 ** 3)}
    sessions = []
    for i in range(N):
        adir = os.path.join(rdir, "s%d" % i)
        os.makedirs(adir)
        env = dict(os.environ,
                   ACOS_ART_DATA=data,
                   ACOS_ART_BIN=WRAP,
                   W43_REAL_ART=ART,
                   W43_SESSION_DIR=adir,
                   ACOS_ART_STAGE_TIMING="1",
                   ACOSLAUNCH_GUEST_STDOUT_FILE=os.path.join(adir, "guest-stdout.log"),
                   ACOSLAUNCH_GUEST_STDERR_FILE=os.path.join(adir, "guest-stderr.log"))
        fh = open(os.path.join(adir, "stdout.json"), "w")
        eh = open(os.path.join(adir, "stderr.txt"), "w")
        p = subprocess.Popen([ACOS, "verify", "--scenario", SCEN, "--apk", APK,
                              "--engine", "art", "--artifacts", adir, "--json"],
                             cwd=ROOT, stdin=subprocess.DEVNULL, stdout=fh, stderr=eh,
                             env=env, start_new_session=True)
        sessions.append({"i": i, "p": p, "adir": adir, "fh": fh, "eh": eh, "samples": []})
    t0 = time.time()
    stop = threading.Event()

    def sampler():
        while not stop.is_set():
            pm = ppid_map()
            for s in sessions:
                try:
                    sample_session(s, pm)
                except Exception:
                    pass
            stop.wait(0.1)

    th = threading.Thread(target=sampler, daemon=True)
    th.start()
    host_mid = None
    for s in sessions:
        try:
            s["rc"] = s["p"].wait(timeout=TIMEOUT)
        except subprocess.TimeoutExpired:
            s["p"].kill(); s["rc"] = "timeout"
        s["fh"].close(); s["eh"].close()
    stop.set(); th.join(timeout=10)
    wall = time.time() - t0
    host_after = {**meminfo(), **loadavg()}
    per = []
    for s in sessions:
        err_code, err_msg = None, ""
        try:
            doc = json.load(open(os.path.join(s["adir"], "stdout.json")))
            e = doc.get("error") or {}
            err_code, err_msg = e.get("code"), e.get("message") or ""
            v = (doc.get("result") or {}).get("verdict") or {}
            passed = doc.get("ok") is True and (v.get("status") or doc.get("status")) == "pass"
            wall_ms = (v.get("resources") or {}).get("wall_ms")
        except Exception:
            passed, wall_ms = False, None
        rc_line = read(os.path.join(s["adir"], "session-exit.txt"))
        m = re.search(r"session_rc=(\d+)", rc_line)
        session_rc = int(m.group(1)) if m else None
        gerr = read(os.path.join(s["adir"], "guest-stderr.log"))
        gout = read(os.path.join(s["adir"], "guest-stdout.log"))
        crash = [l for l in gout.splitlines() if l.startswith("ACOSLAUNCH {")]
        sm = [x for x in s["samples"] if x.get("current")]
        last = sm[-1] if sm else {}
        per.append({
            "i": s["i"], "rc": s["rc"], "pass": passed, "wall_ms": wall_ms,
            "error_code": err_code,
            "error_first_line": (err_msg.splitlines() or [""])[0][:400],
            "error_tail": err_msg[-600:],
            "session_rc": session_rc,
            "guest_crash": crash[-1][:600] if crash else None,
            "guest_stderr_tail": gerr[-800:],
            "guest_stdout_lines": len(gout.splitlines()),
            "cgroup_peak_mib": round((max([x["current"] or 0 for x in sm]) if sm else 0) / MiB, 2),
            "cgroup_last": {k: v for k, v in last.items() if k != "t"},
            "cgroup_samples": len(sm),
            "session_stderr_tail": read(os.path.join(s["adir"], "session-stderr.log"))[-1200:],
        })
    # Disk evidence + prune: the 2026-11-12 ENOSPC window made "session exited"
    # look like a load failure; record free space and flag ENOSPC explicitly.
    du = shutil.disk_usage(OUT)
    disk_full = any("No space left" in (p["session_stderr_tail"] or "") + (p["error_tail"] or "")
                    for p in per)
    reps.append({"rep": rep, "wall_s": round(wall, 3),
                 "disk_free_gib_before": round(host_before.get("disk_free_gib", 0), 2),
                 "disk_free_gib_after": round(du.free / (1024.0 ** 3), 2),
                 "disk_full_seen": disk_full,
                 "host_before": host_before, "host_after": host_after,
                 "passed": sum(1 for p in per if p["pass"]),
                 "sessions": per})
    # keep only the small evidence: per-session JSON/log text, never frames or
    # the shared /data tree (that is what filled the host disk on 2026-11-12).
    shutil.rmtree(data, ignore_errors=True)
    for s in sessions:
        for root_dir, _dirs, files in os.walk(s["adir"]):
            for name in files:
                if name.endswith((".png", ".bin", ".dex", ".so", ".apk", ".idmap")):
                    try:
                        os.remove(os.path.join(root_dir, name))
                    except OSError:
                        pass
    ok = reps[-1]["passed"]
    print("rep %d: %d/%d passed in %.1fs (load1 %.2f->%.2f)"
          % (rep, ok, N, wall, host_before.get("load1", 0), host_after.get("load1", 0)),
          file=sys.stderr, flush=True)

for sp in spinners:
    sp.terminate()
for sp in spinners:
    try:
        sp.wait(timeout=5)
    except subprocess.TimeoutExpired:
        sp.kill()

total = sum(r["passed"] for r in reps)
sess = sum(len(r["sessions"]) for r in reps)
fails = [dict(r=r["rep"], **s) for r in reps for s in r["sessions"] if not s["pass"]]
codes = {}
for f in fails:
    key = (f["error_code"], f["guest_crash"], f["session_rc"])
    codes[key] = codes.get(key, 0) + 1
doc = {"utc_start": cfg["utc_start"], "utc_end": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
       "n": N, "reps": REPS, "spinners": SPINNERS,
       "total_sessions": sess, "total_passed": total,
       "pass_rate": round(100.0 * total / sess, 2) if sess else 0,
       "internal_failures": sum(1 for f in fails if f["error_code"] == "INTERNAL"),
       "failure_classes": [{"error_code": k[0], "guest_crash": k[1], "session_rc": k[2], "count": v}
                           for k, v in codes.items()],
       "reps": reps}
with open(os.path.join(OUT, "run.json"), "w") as f:
    json.dump(doc, f, indent=1)
print("%d/%d sessions passed (%.2f%%), %d INTERNAL, output %s"
      % (total, sess, doc["pass_rate"], doc["internal_failures"], OUT), file=sys.stderr)
PY

python3 - "$OUT" "$ROOT" "$ACOS" "$ART" "$APK" "$SCEN" "$N" "$REPS" "$SPINNERS" "$TIMEOUT" <<'PY'
import json, subprocess, sys, time, os
out = sys.argv[1]
cfg = {"out": out, "root": sys.argv[2], "acos": sys.argv[3], "art": sys.argv[4],
       "apk": sys.argv[5], "scenario": sys.argv[6], "n": int(sys.argv[7]),
       "reps": int(sys.argv[8]), "spinners": int(sys.argv[9]),
       "timeout": int(sys.argv[10]),
       "utc_start": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime())}
with open(os.path.join(out, "cfg.json"), "w") as f:
    json.dump(cfg, f, indent=1)
sys.exit(subprocess.call(["python3", "-I", os.path.join(out, "driver.py"),
                          os.path.join(out, "cfg.json")]))
PY
