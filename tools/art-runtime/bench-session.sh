#!/usr/bin/env bash
#
# bench-session.sh -- W12: the ACTUAL per-session cost of one real interactive
# `acos verify --engine art` session (Option A: stock ViewRootImpl/BLAST/HWUI
# above the ACOS C1 + window session), measured at N=1/N=10 (N=20 optional).
#
#   tools/art-runtime/bench-session.sh [--n "1 10"] [--modes "full launch render"]
#                                      [--reps 1] [--sample-ms 100] [--out DIR]
#                                      [--min-avail-gib 9] [--max-used 75]
#                                      [--keep-frames] [--no-build]
#                                      [--profile release|debug] [--target-dir DIR]
#                                      [--allow-debug] [--analyze DIR]
#
# Build profile (W29): RELEASE ONLY for verdicts. The binaries are read from
#   ${CARGO_TARGET_DIR:-target}/<profile>/{acos,acos-art}  (override: --target-dir,
#   --profile). A debug run is refused unless --allow-debug is passed; an allowed
#   debug run is written with provenance.verdict_eligible=false and the reason
#   "debug build: NOT_VERDICT_ELIGIBLE", so its numbers can never be cited as
#   a D2 verdict. tools/art-runtime/bench-release.sh builds release then runs.
#   Every result JSON stamps profile, full binary sha256 per binary, the
#   git-status porcelain hash, rustc, and the host gate
#   (tools/bench/benchlib.py; BENCHMARKS.md section 11).
#
# Modes
#   full      N concurrent `acos verify --engine art --scenario
#             scenarios/v2-art-counter.yaml` runs: launch -> live tree -> tap
#             Increment (0->1) -> capture -> tap Reset -> tap Increment -> 1 ->
#             teardown. This is the product path (Option A window/surface stack:
#             stock ViewRootImpl + Choreographer + BLAST + HWUI on the Rust C1 +
#             IWindowSession).
#   launch    N concurrent `acos-art launch --until activity-resumed` (one-shot,
#             ACOSLAUNCH_HOLD_MS=2500): the SAME window/surface stack as `full`
#             (C1 + IWindowSession + BLAST) but no interactive extras (no live
#             probe loop, no InputHost taps, no C1 captures/PNGs). One-shot, so
#             its cost is a PEAK over the run.
#   render    N concurrent `acos-art render-app` (one-shot): real Activity on real
#             ART with stock libhwui/Skia, but the NON-windowed path -- a zeroed
#             ViewRootImpl stand-in, a software Bitmap, NO BLAST/BufferQueue/C1/
#             window session. One-shot, PEAK.
#   Decomposition (all real ART, same APK, same host, same sampler):
#     full - launch  = the interactive extras (live probe, InputHost, captures)
#     launch - render = Option A's window/surface stack + the real traversal
#                        (real ViewRootImpl/BLAST path instead of the stand-in)
#   `acos-art session` (the older headless line-JSON session) was tried as the
#   non-windowed comparator and exits rc=1 after activity-resumed at HEAD
#   da9e9aa, before answering one command -- recorded as a finding, not worked
#   around here (crates/ is read-only for this task).
#
# Sampling (python3 -I driver, one process, the whole cohort in one guard):
#   per session, every --sample-ms, over the process tree of that session's
#   `acos verify` / `acos-art session` process:
#     PSS / Pss_Anon / Pss_File / Pss_Shmem / Private_Dirty / Private_Clean /
#     Shared_Clean+Dirty / Rss from /proc/<pid>/smaps_rollup, split by role
#     (guest dalvikvm64, launcher acos-art incl. ServiceHost+C1+window session,
#     bwrap, systemd-run scope shell, the `acos` CLI itself), threads, open fds,
#     live `acos-gralloc` memfds and their mapped bytes (C1-held buffers),
#     CPU time (utime+stime, summed over every pid the session ever had).
#   per session cgroup (the guest's own systemd scope): memory.current (peak of
#   the samples), memory.peak (kernel), memory.max, memory.stat anon/file/shmem,
#   cpu.stat usage_usec, memory.events oom/oom_kill.
#   whole host: MemTotal/MemAvailable every sample (used delta vs idle), plus
#   two full host PSS scans (sum of every /proc/*/smaps_rollup Pss): idle before
#   the cohort and once while all N sessions are up.
#
# Correctness: every session's verdict is read back (status, steps, tap frame
# sequence numbers); a cohort is reported failing if any session is not
# `status: pass` or a tap did not advance the frame/tree. Nothing is taken from
# an engine's own "ok" alone in the comparator modes (the frame/count is read back).
#
# Safety: the whole run holds the host-wide ART lock
# (tools/art-runtime/run-exclusive.sh) and runs under tools/guard/guarded.sh --mem.
# `free -m` MemAvailable is checked before every cohort and the projected peak
# is refused above --max-used % of MemTotal (default 75). ramguard.sh is started
# if it is not already running and is stopped again if this script started it.
# Host RAM must never exceed 90 % (docs/OPERATING_DIRECTIVES.md section 5).
# Scratch lives under .cache/ (never /tmp).
#
# Output (default .cache/bench-session/<UTC stamp>/):
#   session-<mode>-N<n>/rep-<k>/session-<i>/       per-session verify/session artifacts
#   session-<mode>-N<n>/rep-<k>/samples.jsonl     sampler rows
#   session-<mode>-N<n>/rep-<k>/summary.json      cohort + per-session metrics
#   summary.json                                  all cohorts (this is the file to cite)
#   table.md                                      the printed table
#
# One command re-runs the whole harness (W11 release-path change, or later):
#   tools/art-runtime/bench-session.sh --out results/bench/w12-rerun
#
# Re-analysis (no ART run): `--analyze DIR` re-derives the tables/comparisons from
# a recorded DIR/summary.json and rewrites DIR/table.md. The per-session rows it
# needs are already in that file, so this cannot invent a number.
set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT" || exit 1

usage() { sed -n '2,70p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'; }

NS="1 10"
MODES="full launch render"
REPS=1
SAMPLE_MS=100
OUT=""
MIN_AVAIL_GIB=9
MAX_USED=75
KEEP_FRAMES=0
NO_BUILD=0
ANALYZE=""
GUARD_MEM=""
PROFILE="${ACOS_BENCH_PROFILE:-release}"
TARGET_DIR="${CARGO_TARGET_DIR:-target}"
ALLOW_DEBUG=0
while [ $# -gt 0 ]; do
    case "$1" in
        --n)             NS="$2"; shift 2 ;;
        --modes)         MODES="$2"; shift 2 ;;
        --reps)          REPS="$2"; shift 2 ;;
        --sample-ms)     SAMPLE_MS="$2"; shift 2 ;;
        --out)           OUT="$2"; shift 2 ;;
        --min-avail-gib) MIN_AVAIL_GIB="$2"; shift 2 ;;
        --max-used)      MAX_USED="$2"; shift 2 ;;
        --keep-frames)   KEEP_FRAMES=1; shift ;;
        --analyze)       ANALYZE="$2"; shift 2 ;;
        --no-build)      NO_BUILD=1; shift ;;
        --mem)           GUARD_MEM="$2"; shift 2 ;;
        --profile)       PROFILE="$2"; shift 2 ;;
        --target-dir)    TARGET_DIR="$2"; shift 2 ;;
        --allow-debug)   ALLOW_DEBUG=1; shift ;;
        -h|--help)       usage; exit 0 ;;
        *) echo "bench-session: unknown argument: $1" >&2; exit 2 ;;
    esac
done

for m in $MODES; do
    case "$m" in full|launch|render) ;; *) echo "bench-session: mode must be full|launch|render (got '$m')" >&2; exit 2 ;; esac
done
case "$NS" in ''|*[!0-9\ ]*) echo "bench-session: --n must be a space-separated list of integers" >&2; exit 2 ;; esac

STAMP="$(date -u +%Y%m%dT%H%M%SZ)"
[ -n "$OUT" ] || OUT="$ROOT/.cache/bench-session/$STAMP"
mkdir -p "$OUT"

case "$PROFILE" in
    release|debug) ;;
    *) echo "bench-session: --profile must be release or debug (got '$PROFILE')" >&2; exit 2 ;;
esac
case "$TARGET_DIR" in
    /*) BIN_ROOT="$TARGET_DIR" ;;
    *)  BIN_ROOT="$ROOT/$TARGET_DIR" ;;
esac
if [ "$PROFILE" != "release" ] && [ "$ALLOW_DEBUG" -eq 0 ]; then
    echo "bench-session: refusing a $PROFILE build (verdicts are release-only)." >&2
    echo "bench-session: rebuild with tools/art-runtime/bench-release.sh, or pass" >&2
    echo "bench-session: --allow-debug to record explicitly NOT_VERDICT_ELIGIBLE rows." >&2
    exit 5
fi

APK="fixtures/build/apps/counter.apk"
SCENARIO="scenarios/v2-art-counter.yaml"
ACOS="$BIN_ROOT/$PROFILE/acos"
ACOS_ART="$BIN_ROOT/$PROFILE/acos-art"

# Host gate (loadavg1 <= 4.0, MemAvailable >= 30 GiB). The ART lock is taken by
# run-exclusive.sh at the end of this script, so the probe here skips the lock.
if ! python3 -I - "$ROOT" <<'PY'
import sys
sys.path.insert(0, sys.argv[1] + "/tools/bench")
import benchlib
g = benchlib.host_gate(sys.argv[1], check_lock=False)
print("bench-session: host gate %s (loadavg1 %s, MemAvailable %s GiB)"
      % ("ok" if g["ok"] else "REFUSED", g["loadavg1"], g["mem_available_gib"]),
      file=sys.stderr)
if not g["ok"]:
    for r in g["reasons"]:
        print("bench-session:   " + r, file=sys.stderr)
    sys.exit(1)
PY
then
    exit 4
fi

if [ ! -f "$APK" ]; then echo "bench-session: $APK missing (run tools/build/build-apps.sh)" >&2; exit 2; fi
if [ ! -f "$SCENARIO" ]; then echo "bench-session: $SCENARIO missing" >&2; exit 2; fi

# ---- build (guarded); --no-build reuses existing binaries of this profile ----
BUILD_ARGS=(-p acos-cli -p acos-art)
[ "$PROFILE" = release ] && BUILD_ARGS=(--release "${BUILD_ARGS[@]}")
if [ "$NO_BUILD" -eq 0 ]; then
    if ! CARGO_TARGET_DIR="$TARGET_DIR" "$ROOT/tools/guard/guarded.sh" --mem 12G --time 900 -- \
            cargo build "${BUILD_ARGS[@]}" >"$OUT/build.log" 2>&1; then
        echo "bench-session: build failed; see $OUT/build.log" >&2
        exit 3
    fi
fi
for b in "$ACOS" "$ACOS_ART"; do
    [ -x "$b" ] || { echo "bench-session: $b not built" >&2; exit 3; }
done

# ---- host safety: MemAvailable, projection, ramguard ----
read -r MEM_TOTAL_KIB MEM_AVAIL_KIB < <(awk '/^MemTotal:/{t=$2} /^MemAvailable:/{a=$2} END{print t, a}' /proc/meminfo)
USED_PCT=$(( (MEM_TOTAL_KIB - MEM_AVAIL_KIB) * 100 / MEM_TOTAL_KIB ))
if [ "$USED_PCT" -ge "$MAX_USED" ]; then
    echo "bench-session: host RAM used ${USED_PCT}% >= ${MAX_USED}% before start; refusing" >&2
    exit 4
fi
if [ $(( MEM_AVAIL_KIB / 1024 / 1024 )) -lt "$MIN_AVAIL_GIB" ]; then
    echo "bench-session: MemAvailable below ${MIN_AVAIL_GIB} GiB; refusing" >&2
    exit 4
fi

RAMGUARD_PID=""
if ! pgrep -f 'tools/guard/ramguard.sh' >/dev/null 2>&1; then
    "$ROOT/tools/guard/ramguard.sh" --min-avail-gib 9 >/dev/null 2>&1 &
    RAMGUARD_PID=$!
    echo "bench-session: started ramguard pid=$RAMGUARD_PID" >&2
    sleep 0.5
else
    echo "bench-session: ramguard already running" >&2
fi
cleanup() {
    if [ -n "$RAMGUARD_PID" ]; then kill "$RAMGUARD_PID" 2>/dev/null; fi
}
trap cleanup EXIT

# ---- the measurement driver ----
cat >"$OUT/driver.py" <<'PY'
import datetime
import glob
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

_ARGV = sys.argv[1:]
if _ARGV and _ARGV[0] == "--analyze":
    # Re-analysis mode: no measurement, no globals that point at binaries.
    ROOT = OUT = ""
    NS_ARG = MODES_ARG = ""
    REPS_ARG, SAMPLE_MS_ARG, MIN_AVAIL_ARG, MAX_USED_ARG = "1", "100", "9", "75"
    KEEP_FRAMES_ARG = "0"
    PROFILE_ARG, ACOS_ARG, ACOS_ART_ARG = "unknown", "", ""
else:
    (ROOT, OUT, NS_ARG, MODES_ARG, REPS_ARG, SAMPLE_MS_ARG, MIN_AVAIL_ARG,
     MAX_USED_ARG, KEEP_FRAMES_ARG, PROFILE_ARG, ACOS_ARG, ACOS_ART_ARG) = _ARGV[:12]
NS = [int(x) for x in NS_ARG.split()]
MODES = MODES_ARG.split()
REPS = int(REPS_ARG)
SAMPLE_MS = int(SAMPLE_MS_ARG)
MIN_AVAIL_GIB = float(MIN_AVAIL_ARG)
MAX_USED = float(MAX_USED_ARG)
KEEP_FRAMES = KEEP_FRAMES_ARG == "1"
PROFILE = PROFILE_ARG
VERDICT_ELIGIBLE = PROFILE == "release"

# Shared statistics/noise/provenance library (W29). Re-analysis mode does not
# import it: it must never need a binary or a measurement to re-render a table.
benchlib = None
_lib_root = ROOT or os.getcwd()
if os.path.isdir(os.path.join(_lib_root, "tools", "bench")):
    sys.path.insert(0, os.path.join(_lib_root, "tools", "bench"))
    try:
        import benchlib  # noqa: E402
    except Exception as _exc:  # pragma: no cover - defensive
        print("bench-session: benchlib import failed: %s" % _exc, file=sys.stderr)
        raise
APK = os.path.join(ROOT, "fixtures/build/apps/counter.apk")
SCENARIO = os.path.join(ROOT, "scenarios/v2-art-counter.yaml")
ACOS = ACOS_ARG or os.path.join(ROOT, "target/release/acos")
ACOS_ART = ACOS_ART_ARG or os.path.join(ROOT, "target/release/acos-art")
COUNT_SUFFIX = ":id/count"

ROLES = {  # comm -> component label (first match wins)
    "dalvikvm64": "guest",
    "acos-art": "launcher",
    "bwrap": "bwrap",
    "systemd-run": "scope",
    "bash": "scope",
    "sh": "scope",
    "acos": "cli",
}


def now():
    return time.time()


def ms(a, b):
    return round((b - a) * 1000.0, 1)


# ---------------------------------------------------------------- host / proc
def meminfo():
    m = {}
    with open("/proc/meminfo") as f:
        for line in f:
            k, v = line.split(":", 1)
            m[k] = int(v.split()[0])  # kB
    total, avail = m["MemTotal"], m["MemAvailable"]
    return {"total_mib": total / 1024.0, "avail_mib": avail / 1024.0,
            "used_mib": (total - avail) / 1024.0,
            "used_pct": 100.0 * (total - avail) / total}


def ppid_map():
    m = {}
    for name in os.listdir("/proc"):
        if not name.isdigit():
            continue
        try:
            with open("/proc/%s/stat" % name) as f:
                s = f.read()
            ppid = int(s.rsplit(")", 1)[1].split()[1])
            m.setdefault(ppid, []).append(int(name))
        except (OSError, ValueError, IndexError):
            pass
    return m


def tree_of(roots, pm):
    seen, stack = set(), list(roots)
    while stack:
        p = stack.pop()
        if p in seen:
            continue
        seen.add(p)
        stack.extend(pm.get(p, []))
    return seen


def read_text(path):
    try:
        with open(path) as f:
            return f.read()
    except OSError:
        return ""


def comm_of(pid):
    return read_text("/proc/%d/comm" % pid).strip()


SMAPS_KEYS = ("Pss", "Pss_Anon", "Pss_File", "Pss_Shmem", "Private_Dirty",
              "Private_Clean", "Shared_Clean", "Shared_Dirty", "Rss")


def smaps(pid):
    out = dict.fromkeys(SMAPS_KEYS, 0)
    text = read_text("/proc/%d/smaps_rollup" % pid)
    for line in text.splitlines():
        k, _, rest = line.partition(":")
        if k in out:
            try:
                out[k] = int(rest.split()[0])
            except (ValueError, IndexError):
                pass
    return out


def proc_stat(pid):
    """(utime+stime) ticks, threads, state — from /proc/<pid>/stat."""
    s = read_text("/proc/%d/stat" % pid)
    try:
        rest = s.rsplit(")", 1)[1].split()
        st = rest[0]
        utime, stime = int(rest[11]), int(rest[12])
        threads = int(rest[17])
    except (IndexError, ValueError):
        return 0, 0, "?"
    return utime + stime, threads, st


def fd_info(pid):
    """fds, live acos-gralloc memfds, mapped gralloc bytes (deduped by inode)."""
    fds = gralloc_fds = 0
    try:
        fds = len(os.listdir("/proc/%d/fd" % pid))
    except OSError:
        pass
    inodes = {}
    try:
        for entry in os.listdir("/proc/%d/fd" % pid):
            try:
                target = os.readlink("/proc/%d/fd/%s" % (pid, entry))
            except OSError:
                continue
            if "acos-gralloc" in target:
                gralloc_fds += 1
    except OSError:
        pass
    for line in read_text("/proc/%d/maps" % pid).splitlines():
        if "acos-gralloc" not in line:
            continue
        f = line.split()
        if len(f) < 6:
            continue
        try:
            a, b = f[0].split("-")
            size = int(b, 16) - int(a, 16)
        except ValueError:
            continue
        inodes[f[4]] = max(inodes.get(f[4], 0), size)  # one mapping per fd
    return fds, gralloc_fds, sum(inodes.values())


GUEST_COMMS = {"dalvikvm64", "LaunchMain", "SessionMain", "AppMain", "main"}


def role_of(comm, exe=""):
    for key, role in ROLES.items():
        if comm == key:
            return role
    if comm in GUEST_COMMS or "dalvikvm" in comm or exe.endswith("/dalvikvm64"):
        return "guest"
    return "other"


def exe_of(pid):
    try:
        return os.readlink("/proc/%d/exe" % pid)
    except OSError:
        return ""


def cgroup_path_of(pid):
    for line in read_text("/proc/%d/cgroup" % pid).splitlines():
        if line.startswith("0::"):
            return line[3:].strip()
    return None


def cg_read(rel, name, default=0):
    text = read_text(os.path.join("/sys/fs/cgroup", rel.lstrip("/"), name))
    if not text:
        return default
    try:
        return int(text.strip())
    except ValueError:
        return default


def cg_read_map(rel, name, keys):
    out = dict.fromkeys(keys, 0)
    for line in read_text(os.path.join("/sys/fs/cgroup", rel.lstrip("/"), name)).splitlines():
        k, _, v = line.partition(" ")
        if k in out:
            try:
                out[k] = int(v.strip())
            except ValueError:
                pass
    return out


def host_pss_mib():
    """Sum of Pss over every process on the host (whole-host PSS total)."""
    total = 0
    for name in os.listdir("/proc"):
        if not name.isdigit():
            continue
        text = read_text("/proc/%s/smaps_rollup" % name)
        for line in text.splitlines():
            if line.startswith("Pss:"):
                try:
                    total += int(line.split()[1])
                except (ValueError, IndexError):
                    pass
                break
    return total / 1024.0


def stats(xs, digits=2):
    xs = sorted(x for x in xs if x is not None)
    if not xs:
        return None
    return {"n": len(xs), "p50": round(statistics.median(xs), digits),
            "min": round(xs[0], digits), "max": round(xs[-1], digits)}


def gralloc_log(path):
    """(count, MiB) of C1 gralloc allocations logged by the guest shim."""
    count = 0
    total = 0
    for line in read_text(path).splitlines():
        m = re.search(r"allocate:\s+(\d+)x(\d+).*?\s(\d+) B \(", line)
        if m:
            count += 1
            total += int(m.group(3))
    return count, total / (1024.0 * 1024.0)


# ---------------------------------------------------------------- sampler
class Sampler(threading.Thread):
    """Walks every session's process tree at a fixed cadence."""

    def __init__(self, path, table, period_ms):
        super().__init__(daemon=True)
        self.path = path
        self.table = table          # label -> list of root pids (single root each)
        self.period = period_ms / 1000.0
        self.stop = threading.Event()
        self.rows = 0

    def run(self):
        with open(self.path, "w") as f:
            while not self.stop.is_set():
                t = now()
                pm = ppid_map()
                hosts = meminfo()
                row = {"t": t, "host": hosts, "sessions": {}}
                for label in list(self.table):
                    roots = [p for p in self.table[label] if p]
                    if not roots:
                        continue
                    pids = tree_of(roots, pm)
                    if not pids:
                        continue
                    agg = {"pss_kb": 0, "pss_anon_kb": 0, "pss_file_kb": 0,
                           "pss_shmem_kb": 0, "priv_dirty_kb": 0, "priv_clean_kb": 0,
                           "shared_kb": 0, "rss_kb": 0, "threads": 0, "fds": 0,
                           "gralloc_fds": 0, "gralloc_bytes": 0, "cpu_ticks": 0,
                           "nproc": 0, "roles": {}}
                    guest_pid = None
                    comms = []
                    for pid in sorted(pids):
                        comm = comm_of(pid)
                        role = role_of(comm, exe_of(pid))
                        comms.append("%s:%s" % (role, comm))
                        m = smaps(pid)
                        cpu, threads, st = proc_stat(pid)
                        fds, gfd, gbytes = fd_info(pid)
                        if role == "guest" and guest_pid is None and st != "Z":
                            guest_pid = pid
                        e = {"role": role, "comm": comm, "pss_kb": m["Pss"],
                             "pss_anon_kb": m["Pss_Anon"], "pss_file_kb": m["Pss_File"],
                             "pss_shmem_kb": m["Pss_Shmem"],
                             "priv_dirty_kb": m["Private_Dirty"],
                             "priv_clean_kb": m["Private_Clean"],
                             "shared_kb": m["Shared_Clean"] + m["Shared_Dirty"],
                             "rss_kb": m["Rss"], "threads": threads, "fds": fds,
                             "gralloc_fds": gfd, "gralloc_bytes": gbytes,
                             "cpu_ticks": cpu, "state": st}
                        agg["roles"].setdefault(role, {"pss_kb": 0, "priv_dirty_kb": 0,
                                                      "pss_file_kb": 0, "pss_anon_kb": 0,
                                                      "pss_shmem_kb": 0, "gralloc_bytes": 0,
                                                      "threads": 0, "fds": 0, "nproc": 0})
                        r = agg["roles"][role]
                        r["pss_kb"] += m["Pss"]
                        r["priv_dirty_kb"] += m["Private_Dirty"]
                        r["pss_file_kb"] += m["Pss_File"]
                        r["pss_anon_kb"] += m["Pss_Anon"]
                        r["pss_shmem_kb"] += m["Pss_Shmem"]
                        r["gralloc_bytes"] += gbytes
                        r["threads"] += threads
                        r["fds"] += fds
                        r["nproc"] += 1
                        agg["pss_kb"] += m["Pss"]
                        agg["pss_anon_kb"] += m["Pss_Anon"]
                        agg["pss_file_kb"] += m["Pss_File"]
                        agg["pss_shmem_kb"] += m["Pss_Shmem"]
                        agg["priv_dirty_kb"] += m["Private_Dirty"]
                        agg["priv_clean_kb"] += m["Private_Clean"]
                        agg["shared_kb"] += m["Shared_Clean"] + m["Shared_Dirty"]
                        agg["rss_kb"] += m["Rss"]
                        agg["threads"] += threads
                        agg["fds"] += fds
                        agg["gralloc_fds"] += gfd
                        agg["gralloc_bytes"] += gbytes
                        agg["cpu_ticks"] += cpu
                        agg["nproc"] += 1
                    if guest_pid:
                        rel = cgroup_path_of(guest_pid)
                        if rel:
                            agg["cgroup"] = {
                                "path": rel,
                                "current_bytes": cg_read(rel, "memory.current"),
                                "peak_bytes": cg_read(rel, "memory.peak"),
                                "max_bytes": cg_read(rel, "memory.max"),
                                "stat": cg_read_map(rel, "memory.stat",
                                                    ("anon", "file", "shmem", "sock", "kernel")),
                                "cpu_usage_usec": cg_read_map(rel, "cpu.stat",
                                                              ("usage_usec",))["usage_usec"],
                                "events": cg_read_map(rel, "memory.events",
                                                      ("oom", "oom_kill", "high", "max")),
                            }
                    agg["comms"] = sorted(set(comms))
                    row["sessions"][label] = agg
                f.write(json.dumps(row) + "\n")
                f.flush()
                self.rows += 1
                time.sleep(max(0.0, self.period - (now() - t)))

    def finish(self):
        self.stop.set()
        self.join(timeout=10)


# ---------------------------------------------------------------- session procs
def wait_for(pred, timeout):
    end = time.time() + timeout
    while time.time() < end:
        if pred():
            return True
        time.sleep(0.02)
    return False


def find_count(nodes):
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


class FullSession:
    """One `acos verify --engine art` (Option A product path)."""

    mode = "full"

    def __init__(self, label, adir):
        self.label = label
        self.adir = adir
        self.proc = None
        self.out = os.path.join(adir, "stdout.json")
        self.err = os.path.join(adir, "stderr.txt")
        self.guest_log = os.path.join(adir, "guest-stderr.log")
        self.t0 = self.t1 = None
        self.rc = None
        self.reply = None

    def start(self):
        os.makedirs(self.adir, exist_ok=True)
        env = dict(os.environ,
                   ACOSART_HW_VERBOSE="1",
                   ACOSLAUNCH_GUEST_STDERR_FILE=self.guest_log)
        self.t0 = now()
        self.fh = open(self.out, "w")
        self.proc = subprocess.Popen(
            [ACOS, "verify", "--scenario", SCENARIO, "--apk", APK, "--engine", "art",
             "--artifacts", os.path.join(self.adir, "artifacts"), "--json"],
            cwd=ROOT, env=env, stdin=subprocess.DEVNULL, stdout=self.fh,
            stderr=open(self.err, "w"), start_new_session=True)
        return self.proc.pid

    def wait(self):
        self.rc = self.proc.wait()
        self.t1 = now()
        self.fh.close()
        try:
            with open(self.out) as f:
                doc = json.load(f)
        except Exception:
            doc = {}
        res = doc.get("result") or {}
        verdict = res.get("verdict") or {}
        steps = verdict.get("steps") or []
        taps = [s for s in steps if s.get("step") == "tap"]
        cumulative = 0
        first_frame_ms = None
        for s in steps:
            cumulative += s.get("duration_ms") or 0
            if first_frame_ms is None and (s.get("frame_seq") or 0) >= 1:
                first_frame_ms = cumulative
        launch = next((s for s in steps if s.get("step") == "launch"), {})
        self.reply = {
            "status": verdict.get("status") or doc.get("status"),
            "ok": doc.get("ok"),
            "engine": verdict.get("engine"),
            "mode": verdict.get("mode"),
            "unsupported_steps": [s.get("step") for s in steps
                                  if str(s.get("status")) == "unsupported"],
            "wall_ms": (verdict.get("resources") or {}).get("wall_ms"),
            "peak_rss_bytes": (verdict.get("resources") or {}).get("peak_rss_bytes"),
            "memory_mib": (verdict.get("resources") or {}).get("memory_mib"),
            "frames_captured": (verdict.get("resources") or {}).get("frames_captured"),
            "n_steps": len(steps),
            "steps": [{"step": s.get("step"), "status": s.get("status"),
                       "duration_ms": s.get("duration_ms"),
                       "frame_seq": s.get("frame_seq")} for s in steps],
            "launch_step_ms": launch.get("duration_ms"),
            "first_live_frame_ms": first_frame_ms,
            "tap_ms": [t.get("duration_ms") for t in taps],
            "tap_frame_seq": [t.get("frame_seq") for t in taps],
            "harness_wall_ms": ms(self.t0, self.t1),
        }
        return self.reply

    def correctness(self):
        r = self.reply or {}
        steps = r.get("steps") or []
        taps = [s for s in steps if s["step"] == "tap"]
        taps_pass = bool(taps) and all(s.get("status") == "pass" for s in taps)
        frames_advance = all((s.get("frame_seq") or 0) >= 1 for s in taps)
        return {"pass": (r.get("status") == "pass" and r.get("ok") is True
                         and taps_pass and frames_advance),
                "status": r.get("status"), "taps": len(taps),
                "taps_pass": taps_pass, "tap_frames": r.get("tap_frame_seq")}


class OneShotSession:
    """One `acos-art launch` (window stack) or `acos-art render-app` (no window
    stack) run. One-shot: its per-session cost is a peak over the run."""

    def __init__(self, mode, label, adir):
        self.mode = mode
        self.label = label
        self.adir = adir
        self.proc = None
        self.t0 = self.t1 = None
        self.reply = None

    def start(self):
        os.makedirs(self.adir, exist_ok=True)
        out = os.path.join(self.adir, "stdout.json")
        env = dict(os.environ, ACOSART_HW_VERBOSE="1")
        self.guest_log = os.path.join(self.adir, "guest-stderr.log")
        env["ACOSLAUNCH_GUEST_STDERR_FILE"] = self.guest_log
        if self.mode == "launch":
            env["ACOSLAUNCH_HOLD_MS"] = "2500"
            argv = [ACOS_ART, "launch", "--apk", APK, "--until", "activity-resumed"]
        else:
            argv = [ACOS_ART, "render-app", "--apk", APK, "--json",
                    "--out", os.path.join(self.adir, "out")]
        self.t0 = now()
        self.fh = open(out, "w")
        self.proc = subprocess.Popen(
            argv, cwd=ROOT, env=env, stdin=subprocess.DEVNULL, stdout=self.fh,
            stderr=open(os.path.join(self.adir, "stderr.txt"), "w"),
            start_new_session=True)
        return self.proc.pid

    def wait(self):
        try:
            self.rc = self.proc.wait(timeout=300)
        except subprocess.TimeoutExpired:
            os.killpg(self.proc.pid, signal.SIGKILL)
            self.rc = self.proc.wait()
        self.t1 = now()
        self.fh.close()
        try:
            with open(os.path.join(self.adir, "stdout.json")) as f:
                doc = json.load(f)
        except Exception:
            doc = {}
        self.reply = {
            "outcome": doc.get("outcome"),
            "guest_wall_ms": doc.get("wall_ms"),
            "guest_wall_ms_reported": doc.get("wall_ms"),
            "peak_rss_mb_self": doc.get("peak_rss_mb"),
            "nodes": doc.get("nodes"),
            "frame": doc.get("frame"),
            "until": doc.get("until"),
            "harness_wall_ms": ms(self.t0, self.t1),
        }
        return self.reply

    def correctness(self):
        r = self.reply or {}
        ok = r.get("outcome") == "ok"
        if self.mode == "render" and ok:
            ok = bool(r.get("frame")) and (r.get("nodes") or 0) >= 8
        return {"pass": ok, "status": r.get("outcome"),
                "nodes": r.get("nodes"), "until": r.get("until")}


# ---------------------------------------------------------------- cohort
def run_cohort(mode, n, repdir, idle_baseline):
    os.makedirs(repdir, exist_ok=True)
    session_dir = os.path.join(repdir, "sessions")
    os.makedirs(session_dir, exist_ok=True)
    table = {}
    sessions = []
    for i in range(1, n + 1):
        label = "s%d" % i
        adir = os.path.join(session_dir, label)
        os.makedirs(adir, exist_ok=True)
        s = FullSession(label, adir) if mode == "full" else OneShotSession(mode, label, adir)
        sessions.append(s)

    sampler = Sampler(os.path.join(repdir, "samples.jsonl"), table, SAMPLE_MS)
    host = {"idle": idle_baseline, "during": None}
    sampler.start()

    t_cohort0 = now()
    for s in sessions:
        table[s.label] = [s.start()]
    host["during_holder"] = {}

    def host_scan():
        wait_for(lambda: sum(1 for s in sessions if s.proc and s.proc.poll() is None) == n, 240)
        time.sleep(0.3)
        host["during"] = {"pss_mib": host_pss_mib(), "at": now(),
                          "host": meminfo(), "n_alive": n}

    th = threading.Thread(target=host_scan, daemon=True)
    th.start()
    replies = [s.wait() for s in sessions]
    for s in sessions:
        s.correctness()
    if not host["during"]:
        host["during"] = {"pss_mib": host_pss_mib(), "at": now(), "host": meminfo(),
                          "n_alive": 0, "note": "sessions finished before the scan"}

    t_cohort1 = now()
    sampler.finish()

    # ---- post-process the samples ----
    rows = []
    with open(os.path.join(repdir, "samples.jsonl")) as f:
        for line in f:
            line = line.strip()
            if line:
                rows.append(json.loads(line))

    def without_cli(r):
        return r["pss_kb"] - r["roles"].get("cli", {}).get("pss_kb", 0)

    per = {}
    for s in sessions:
        sess_rows = [r["sessions"][s.label] for r in rows if s.label in r["sessions"]]
        # "active" = the guest process exists in that sample (after launch, before exit).
        active = [r for r in sess_rows if r["roles"].get("guest")]
        use = active or sess_rows
        peak_row = max(use, key=without_cli) if use else None
        steady = (statistics.median([without_cli(r) for r in use]) / 1024.0) if use else 0.0
        n_alloc, alloc_mib = gralloc_log(getattr(s, "guest_log", "") or "")
        cg = [r.get("cgroup") for r in use if r.get("cgroup")]
        cg_series = [{"t": r["t"], "bytes": r["sessions"][s.label]["cgroup"]["current_bytes"]}
                     for r in rows if s.label in r["sessions"]
                     and r["sessions"][s.label].get("cgroup")
                     and r["sessions"][s.label]["cgroup"].get("current_bytes") is not None]
        cg_gib_min = (benchlib.sample_integral(cg_series) if benchlib is not None else None)
        cg_peak_current = max((c["current_bytes"] for c in cg), default=0) / (1024.0 * 1024.0)
        cg_kernel_peak = max((c["peak_bytes"] for c in cg), default=0) / (1024.0 * 1024.0)
        cg_max = max((c["max_bytes"] for c in cg), default=0) / (1024.0 * 1024.0)
        cg_stat = cg[-1]["stat"] if cg else {}
        oom = sum(c["events"].get("oom_kill", 0) for c in cg)
        role_peak = {}
        for role in ("guest", "launcher", "bwrap", "scope", "cli", "other"):
            vals = [r["roles"].get(role, {}) for r in use]
            role_peak[role] = {
                "pss_mib": round(max((v.get("pss_kb", 0) for v in vals), default=0) / 1024.0, 2),
                "pss_file_mib": round(max((v.get("pss_file_kb", 0) for v in vals), default=0) / 1024.0, 2),
                "pss_anon_mib": round(max((v.get("pss_anon_kb", 0) for v in vals), default=0) / 1024.0, 2),
                "priv_dirty_mib": round(max((v.get("priv_dirty_kb", 0) for v in vals), default=0) / 1024.0, 2),
                "nproc": max((v.get("nproc", 0) for v in vals), default=0),
            }
        # Components of the peak sample (one consistent row, not per-field maxima).
        def sub(row, role, key):
            return (row["roles"].get(role, {}).get(key, 0) if row else 0)
        peak_fields = {}
        for key, aggkey in (("pss_anon_kb", "pss_anon_kb"), ("pss_file_kb", "pss_file_kb"),
                            ("pss_shmem_kb", "pss_shmem_kb"),
                            ("priv_dirty_kb", "priv_dirty_kb"),
                            ("priv_clean_kb", "priv_clean_kb"),
                            ("shared_kb", "shared_kb"), ("rss_kb", "rss_kb")):
            if peak_row:
                v = peak_row[aggkey] - sum(sub(peak_row, "cli", aggkey) for _ in (0,))
            else:
                v = 0
            peak_fields[key] = round(v / 1024.0, 2)
        per[s.label] = {
            "label": s.label,
            "mode": mode,
            "peak_pss_mib": round(without_cli(peak_row) / 1024.0, 2) if peak_row else 0.0,
            "peak_pss_including_cli_mib": round(peak_row["pss_kb"] / 1024.0, 2) if peak_row else 0.0,
            "steady_pss_mib": round(steady, 2),
            "cli_peak_pss_mib": role_peak["cli"]["pss_mib"],
            "pss_anon_peak_mib": peak_fields["pss_anon_kb"],
            "pss_file_peak_mib": peak_fields["pss_file_kb"],
            "pss_shmem_peak_mib": peak_fields["pss_shmem_kb"],
            "priv_dirty_peak_mib": peak_fields["priv_dirty_kb"],
            "priv_clean_peak_mib": peak_fields["priv_clean_kb"],
            "shared_peak_mib": peak_fields["shared_kb"],
            "rss_peak_mib": peak_fields["rss_kb"] - sub(peak_row, "cli", "rss_kb") / 1024.0
            if peak_row else 0.0,
            "comms": sorted({c for r in use for c in r.get("comms", [])}),
            "roles": role_peak,
            "cgroup": {"peak_current_mib": round(cg_peak_current, 2),
                       "gib_min": round(cg_gib_min, 6) if cg_gib_min is not None else None,
                       "kernel_peak_mib": round(cg_kernel_peak, 2),
                       "max_mib": round(cg_max, 2),
                       "stat_anon_mib": round(cg_stat.get("anon", 0) / (1024.0 * 1024.0), 2),
                       "stat_file_mib": round(cg_stat.get("file", 0) / (1024.0 * 1024.0), 2),
                       "stat_shmem_mib": round(cg_stat.get("shmem", 0) / (1024.0 * 1024.0), 2),
                       "stat_anon_shmem_mib": round((cg_stat.get("anon", 0)
                                                     + cg_stat.get("shmem", 0))
                                                    / (1024.0 * 1024.0), 2),
                       "oom_kills": oom},
            "gralloc_open_fds_peak": max((r["gralloc_fds"] for r in use), default=0),
            "gralloc_alloc_count": n_alloc,
            "gralloc_alloc_mib": round(alloc_mib, 2),
            "threads_peak": max((r["threads"] for r in use), default=0),
            "fds_peak": max((r["fds"] for r in use), default=0),
            "cpu_s": round(max((r["cpu_ticks"] for r in use), default=0)
                           / os.sysconf("SC_CLK_TCK"), 2),
            "process_wall_s": round((s.t1 - s.t0), 3) if s.t1 else None,
            "reply": s.reply,
            "correctness": s.correctness(),
        }

    host_runs = [r["host"] for r in rows]
    peak_host_used = max((h["used_mib"] for h in host_runs), default=0)
    peak_host_pct = max((h["used_pct"] for h in host_runs), default=0)
    summ = {
        "mode": mode, "n": n,
        "cohort_wall_s": round(t_cohort1 - t_cohort0, 3),
        "sessions": per,
        "host": {
            "idle": host["idle"],
            "during": host["during"],
            "peak_used_mib": round(peak_host_used, 1),
            "peak_used_pct": round(peak_host_pct, 2),
            "used_delta_peak_mib": round(peak_host_used - host["idle"]["used_mib"], 1),
            "total_pss_delta_mib": (round(host["during"]["pss_mib"] - host["idle"]["pss_mib"], 1)
                                    if host["during"] else None),
        },
        "totals": {
            "sum_peak_pss_mib": round(sum(p["peak_pss_mib"] for p in per.values()), 2),
            "sum_steady_pss_mib": round(sum(p["steady_pss_mib"] for p in per.values()), 2),
            "sum_guest_peak_pss_mib": round(sum(p["roles"]["guest"]["pss_mib"] for p in per.values()), 2),
            "sum_launcher_peak_pss_mib": round(sum(p["roles"]["launcher"]["pss_mib"] for p in per.values()), 2),
            "sum_cli_peak_pss_mib": round(sum(p["cli_peak_pss_mib"] for p in per.values()), 2),
            "sum_gralloc_alloc_mib": round(sum(p["gralloc_alloc_mib"] for p in per.values()), 2),
            "sum_cgroup_peak_current_mib": round(sum(p["cgroup"]["peak_current_mib"] for p in per.values()), 2),
            "sum_cgroup_gib_min": round(sum((p["cgroup"].get("gib_min") or 0.0)
                                            for p in per.values()), 6),
            "sum_cpu_s": round(sum(p["cpu_s"] for p in per.values()), 2),
        },
        "correctness": {
            "passed": sum(1 for p in per.values() if p["correctness"]["pass"]),
            "total": len(per),
            "oom_kills": sum(p["cgroup"]["oom_kills"] for p in per.values()),
            "failures": [p["label"] for p in per.values() if not p["correctness"]["pass"]],
        },
        "samples": len(rows),
    }
    if not KEEP_FRAMES:
        for path in glob.glob(os.path.join(session_dir, "*", "**", "*.png"), recursive=True):
            try:
                os.unlink(path)
            except OSError:
                pass
    with open(os.path.join(repdir, "summary.json"), "w") as f:
        json.dump(summ, f, indent=2, sort_keys=True)
    return summ


def sha256_16(path):
    try:
        h = hashlib.sha256()
        with open(path, "rb") as f:
            for chunk in iter(lambda: f.read(1 << 20), b""):
                h.update(chunk)
        return h.hexdigest()[:16]
    except OSError:
        return None


def pool_setting():
    """The BLAST pool depth this commit uses (W10 workaround) + env override."""
    const = None
    try:
        with open(os.path.join(ROOT, "crates/acos-services/src/compositor_client.rs")) as f:
            m = re.search(r"DEFAULT_MAX_ACQUIRED_BUFFERS\s*:\s*u32\s*=\s*(\d+)", f.read())
            const = int(m.group(1)) if m else None
    except OSError:
        pass
    env = os.environ.get("ACOS_MAX_ACQUIRED_BUFFERS")
    return {"source_constant": const,
            "env_override": env,
            "effective_max_acquired": int(env) if env and env.isdigit() else const,
            "note": "C1 answers getMaxAcquiredBufferCount; BLAST pool = maxDequeued(2) + this"}


def git(*args):
    try:
        r = subprocess.run(["git", *args], cwd=ROOT, capture_output=True, text=True, timeout=30)
        return r.stdout.strip()
    except Exception:
        return ""


def provenance():
    """Provenance block. W29: benchlib stamps profile + full binary sha256 +
    git-status hash + rustc + host gate; a non-release profile is recorded as
    NOT_VERDICT_ELIGIBLE (the shell refuses debug unless --allow-debug)."""
    gate = None
    lib = None
    if benchlib is not None:
        gate = benchlib.host_gate(ROOT, check_lock=False)
        lib = {
            "schema": "acos-bench-provenance/1",
            "profile": PROFILE,
            "verdict_eligible": VERDICT_ELIGIBLE,
            "not_verdict_eligible_reason": (None if VERDICT_ELIGIBLE else
                                            "%s build: NOT_VERDICT_ELIGIBLE "
                                            "(verdicts require a release build)" % PROFILE),
            "binaries": [benchlib.binary_provenance(ACOS, PROFILE),
                         benchlib.binary_provenance(ACOS_ART, PROFILE)],
            "git": {
                "head": git("rev-parse", "HEAD"),
                "head_subject": git("log", "-1", "--format=%s"),
                "status_porcelain_sha256": benchlib.repo_dirty_hash(ROOT)[0],
                "dirty_files": len([l for l in git("status", "--porcelain").splitlines() if l.strip()]),
            },
            "rustc": benchlib.rustc_version(),
            "host_gate": gate,
            "method": "BENCHMARKS.md section 11 (tools/bench/benchlib.py)",
        }
    return {
        "utc": datetime.datetime.now(datetime.timezone.utc).isoformat(timespec="seconds"),
        "git_head": git("rev-parse", "HEAD"),
        "git_head_subject": git("log", "-1", "--format=%s"),
        "git_status_porcelain_sha256": (benchlib.repo_dirty_hash(ROOT)[0]
                                        if benchlib is not None else None),
        "crates_dirty_files": len([l for l in git("status", "--porcelain", "crates").splitlines() if l.strip()]),
        "profile": PROFILE,
        "verdict_eligible": VERDICT_ELIGIBLE,
        "not_verdict_eligible_reason": (None if VERDICT_ELIGIBLE else
                                        "%s build: NOT_VERDICT_ELIGIBLE" % PROFILE),
        "acos_binary": os.path.relpath(ACOS, ROOT) if ACOS else None,
        "acos_sha256": (benchlib.sha256_file(ACOS) if benchlib is not None else sha256_16(ACOS)),
        "acos_sha256_16": sha256_16(ACOS),
        "acos_art_binary": os.path.relpath(ACOS_ART, ROOT) if ACOS_ART else None,
        "acos_art_sha256": (benchlib.sha256_file(ACOS_ART) if benchlib is not None else sha256_16(ACOS_ART)),
        "acos_art_sha256_16": sha256_16(ACOS_ART),
        "apk": "fixtures/build/apps/counter.apk",
        "apk_sha256_16": sha256_16(APK),
        "scenario": "scenarios/v2-art-counter.yaml",
        "scenario_sha256_16": sha256_16(SCENARIO),
        "pool": pool_setting(),
        "fidelity": {"full": "real-ART interactive (stock ViewRootImpl/Choreographer/BLAST/HWUI on the "
                             "ACOS C1 + IWindowSession); ART reference, not a device/emulator run",
                     "render": "real-ART framework Activity WITHOUT the window stack (zeroed "
                               "ViewRootImpl stand-in, software Bitmap via libhwui, no BLAST/C1) -- "
                               "a memory comparator only, never verification evidence"},
        "pool_note": "BLAST pool depth is recorded under provenance.pool; W10 answered "
                     "getMaxAcquiredBufferCount = 24 (workaround) at commit da9e9aa",
        "memory_types": "Pss / Pss_Anon / Pss_File / Pss_Shmem / Private_Dirty / Shared (sum) from "
                        "smaps_rollup, summed over each session's process tree; cgroup memory.current/"
                        "memory.peak of the guest's own scope; host used = MemTotal-MemAvailable; "
                        "host PSS = sum of Pss over every host process. Never summed across types.",
        "host": {**meminfo(), "nproc": os.cpu_count(), "loadavg": open("/proc/loadavg").read().split()[:3]},
        "shared_lib": lib,
    }


def _med(vals):
    vals = [v for v in vals if v is not None]
    return round(statistics.median(vals), 2) if vals else None


def memory_columns(cohort):
    """W29 same-type memory columns (BENCHMARKS.md section 11, item 6).

    Columns, never summed across types:
      * cgroup memory.current peak per session (sampled every SAMPLE_MS)
      * cgroup GiB-min integral (per job window, trapezoid over memory.current)
      * PSS per session (secondary, share-aware)
      * cgroup memory.stat anon+shmem -- THE legacy-comparable type -- only at
        a 60 s settle after first frame. bench-session's `acos verify` sessions
        exit before 60 s, so that column is reported as NOT_MEASURED here and
        the legacy comparison renders NOT_COMPARABLE (never a win).
    """
    sessions = cohort["sessions"]
    gib_min = [v.get("cgroup", {}).get("gib_min") for v in sessions.values()]
    gib_min = [g for g in gib_min if g is not None]
    pss = [v.get("peak_pss_mib") for v in sessions.values()]
    at_exit = [v.get("cgroup", {}).get("stat_anon_shmem_mib") for v in sessions.values()]
    at_exit = [a for a in at_exit if a is not None]
    cols = {
        "cgroup_peak_mib_per_session": _med([v["cgroup"]["peak_current_mib"]
                                             for v in sessions.values()]),
        "cgroup_gib_min_total": round(sum(gib_min), 6) if gib_min else None,
        "pss_mib_per_session_secondary": _med(pss),
        "anon_shmem_at_exit_mib_per_session": _med(at_exit),
        "anon_shmem_settled_mib_per_session": None,
        "settle_s": None,
        "metric_type": "CGROUP_ANON_SHMEM",
        "state": "cold-process",
        "note": "only anon+shmem settled 60 s after first frame is comparable with the legacy "
                "ACVM section 3 value; the verify session exits before 60 s, so the comparable "
                "column is NOT_MEASURED and any legacy ratio is NOT_COMPARABLE. cgroup peak, "
                "GiB-min and PSS are separate columns and are never added to anon+shmem.",
    }
    if benchlib is not None:
        acos_row = {"value_mib": cols["anon_shmem_settled_mib_per_session"],
                    "metric_type": "CGROUP_ANON_SHMEM", "state": "cold-process",
                    "settle_s": None, "n": len(sessions)}
        cols["legacy_comparison"] = benchlib.compare_memory(
            acos_row, benchlib.legacy_memory_reference())
    return cols


def derive(run):
    """Per-session totals, marginals, the Option A / interactive deltas and the
    W29 same-type memory columns / repeated-rep statistics."""
    cohorts = run["cohorts"]
    prov = run.get("provenance") or {}
    for c in cohorts:
        s = c["sessions"]
        per_total = [v["cgroup"]["peak_current_mib"] + v["roles"]["launcher"]["pss_mib"]
                     + v["roles"]["bwrap"]["pss_mib"] for v in s.values()]
        taps = [t for v in s.values() for t in ((v["reply"] or {}).get("tap_ms") or [])]
        # Only the interactive path reports a launch-to-first-frame step; the one-shot
        # modes swallow the ACOSLAUNCH_HOLD_MS hold in their own guest wall time.
        launch_ms = [(v["reply"] or {}).get("launch_step_ms") for v in s.values()]
        walls = [v["process_wall_s"] for v in s.values()]
        c["derived"] = {
            "per_session_total_mib_median": _med(per_total),
            "per_session_total_mib_max": round(max(per_total), 2) if per_total else None,
            "per_session_guest_cgroup_peak_mib_median": _med(
                [v["cgroup"]["peak_current_mib"] for v in s.values()]),
            "per_session_pss_mib_median": _med([v["peak_pss_mib"] for v in s.values()]),
            "per_session_cli_pss_mib_median": _med([v["cli_peak_pss_mib"] for v in s.values()]),
            "tap_ms_median": _med(taps),
            "tap_ms_max": round(max(taps), 1) if taps else None,
            "first_frame_ms_median": _med(launch_ms),
            "session_wall_s_median": _med(walls),
            "memory_columns": memory_columns(c),
            "profile": prov.get("profile"),
            "verdict_eligible": prov.get("verdict_eligible"),
            "note": "per-session total = guest cgroup memory.peak + launcher PSS + bwrap PSS "
                    "(cgroup counts shared sysroot file pages once per cgroup, so it is an upper "
                    "bound when sessions share them; the PSS columns divide shared pages instead).",
        }
        c["derived"]["memory"] = {
            "metric_type": "CGROUP_ANON_SHMEM",
            "state": "cold-process",
            "settle_s": None,
            "cgroup_peak_mib": c["derived"]["memory_columns"]["cgroup_peak_mib_per_session"],
            "cgroup_gib_min": c["derived"]["memory_columns"]["cgroup_gib_min_total"],
            "pss_mib": c["derived"]["memory_columns"]["pss_mib_per_session_secondary"],
            "anon_shmem_settled_mib": None,
            "anon_shmem_at_exit_mib": c["derived"]["memory_columns"][
                "anon_shmem_at_exit_mib_per_session"],
            "legacy_comparison": c["derived"]["memory_columns"].get("legacy_comparison"),
        }
    # marginal per session from the cohort sums
    for mode in sorted({c["mode"] for c in cohorts}):
        rows = sorted([c for c in cohorts if c["mode"] == mode], key=lambda c: c["n"])
        for prev, cur in zip(rows, rows[1:]):
            cur["derived"]["marginal_pss_mib_per_session"] = round(
                (cur["totals"]["sum_peak_pss_mib"] - prev["totals"]["sum_peak_pss_mib"])
                / (cur["n"] - prev["n"]), 2)
            cur["derived"]["marginal_cgroup_peak_mib_per_session"] = round(
                (cur["totals"]["sum_cgroup_peak_current_mib"]
                 - prev["totals"]["sum_cgroup_peak_current_mib"]) / (cur["n"] - prev["n"]), 2)
            cur["derived"]["marginal_host_used_mib_per_session"] = round(
                (cur["host"]["peak_used_mib"] - prev["host"]["peak_used_mib"])
                / (cur["n"] - prev["n"]), 2)
            cur["derived"]["marginal_from_mode_n"] = "%s N=%d" % (mode, prev["n"])
    by = {(c["mode"], c["n"]): c for c in cohorts}
    # W29: repeated-rep statistics per (mode, N, metric). One rep has no spread;
    # benchlib.is_noisy() then reports the row as unmeasurable, so a single rep
    # can never produce a PASS. These blocks are the raw material for the
    # benchlib.verdict() call once a legacy row of the same type/state exists.
    for mode in sorted({c["mode"] for c in cohorts}):
        for n in sorted({c["n"] for c in cohorts if c["mode"] == mode}):
            reps = [c for c in cohorts if c["mode"] == mode and c["n"] == n]
            if len(reps) < 1 or benchlib is None:
                continue
            metrics = {}
            for key, getter in (
                ("cgroup_peak_mib_per_session",
                 lambda c: c["derived"]["memory_columns"]["cgroup_peak_mib_per_session"]),
                ("pss_mib_per_session",
                 lambda c: c["derived"]["memory_columns"]["pss_mib_per_session_secondary"]),
                ("cgroup_gib_min_total",
                 lambda c: c["derived"]["memory_columns"]["cgroup_gib_min_total"]),
                ("tap_ms_median", lambda c: c["derived"]["tap_ms_median"]),
                ("first_frame_ms_median", lambda c: c["derived"]["first_frame_ms_median"]),
                ("session_wall_s_median", lambda c: c["derived"]["session_wall_s_median"]),
            ):
                vals = [getter(c) for c in reps]
                st = benchlib.stats([v for v in vals if v is not None])
                st["noisy"] = benchlib.is_noisy(st)
                st["reps"] = len(reps)
                metrics[key] = st
            reps[-1]["derived"]["rep_stats"] = metrics
    comp = []
    for n in sorted({c["n"] for c in cohorts}):
        full, launch, render = by.get(("full", n)), by.get(("launch", n)), by.get(("render", n))
        row = {"n": n}
        if launch and render:
            row["option_a_window_stack"] = {
                "pss_delta_mib": round(launch["derived"]["per_session_pss_mib_median"]
                                       - render["derived"]["per_session_pss_mib_median"], 2),
                "cgroup_peak_delta_mib": round(
                    launch["derived"]["per_session_guest_cgroup_peak_mib_median"]
                    - render["derived"]["per_session_guest_cgroup_peak_mib_median"], 2),
                "gralloc_alloc_mib_launch": launch["derived"]["per_session_guest_cgroup_peak_mib_median"],
                "launch_cgroup_peak_mib": launch["derived"]["per_session_guest_cgroup_peak_mib_median"],
                "render_cgroup_peak_mib": render["derived"]["per_session_guest_cgroup_peak_mib_median"],
                "method": "acos-art launch --until activity-resumed (window stack) minus "
                          "acos-art render-app (no window stack), same APK/host/sampler",
            }
        if full and launch:
            row["interactive_extras"] = {
                "pss_delta_mib": round(full["derived"]["per_session_pss_mib_median"]
                                       - launch["derived"]["per_session_pss_mib_median"], 2),
                "cgroup_peak_delta_mib": round(
                    full["derived"]["per_session_guest_cgroup_peak_mib_median"]
                    - launch["derived"]["per_session_guest_cgroup_peak_mib_median"], 2),
                "gralloc_alloc_count_full_median": _med(
                    [v["gralloc_alloc_count"] for v in full["sessions"].values()]),
                "gralloc_alloc_count_launch_median": _med(
                    [v["gralloc_alloc_count"] for v in launch["sessions"].values()]),
                "method": "acos verify --engine art (interactive: live probe, InputHost, C1 "
                          "captures) minus acos-art launch (same window stack, no interaction)",
            }
        comp.append(row)
    run["comparison"] = comp


def fmt(v, d=1):
    if v is None:
        return "n/a"
    if isinstance(v, float):
        return "%.*f" % (d, v)
    return str(v)


def render_table(run):
    out = ["## W12 per-session cost (`bench-session.sh`)", ""]
    out.append("| cohort | N | sessions pass | sum peak PSS MiB | sum steady PSS MiB | guest peak MiB "
               "| launcher peak MiB | Pss_File peak MiB | Private_Dirty peak MiB | gralloc peak MiB "
               "| guest cgroup peak MiB | host used Δ MiB | host PSS Δ MiB | cohort wall s |")
    out.append("|---|" + "---|" * 13)
    for c in run["cohorts"]:
        t = c["totals"]
        s = c["sessions"]
        def med(key):
            vals = [v[key] for v in s.values()]
            return statistics.median(vals) if vals else 0.0
        out.append("| %s | %d | %d/%d | %.1f | %.1f | %.1f | %.1f | %.1f | %.1f | %.1f | %.1f | %.1f | %s | %.2f |" % (
            c["mode"], c["n"], c["correctness"]["passed"], c["correctness"]["total"],
            t["sum_peak_pss_mib"], t["sum_steady_pss_mib"], t["sum_guest_peak_pss_mib"],
            t["sum_launcher_peak_pss_mib"],
            statistics.median([v["pss_file_peak_mib"] for v in s.values()]),
            statistics.median([v["priv_dirty_peak_mib"] for v in s.values()]),
            t["sum_gralloc_alloc_mib"], t["sum_cgroup_peak_current_mib"],
            c["host"]["used_delta_peak_mib"], fmt(c["host"]["total_pss_delta_mib"]),
            c["cohort_wall_s"]))
    out.append("")
    out.append("")
    out.append("| cohort | per-session total MiB (cg+launcher+bwrap) | per-session PSS MiB | guest cgroup peak MiB "
               "| CLI PSS MiB | tap ms median | first-frame ms | session wall s | marginal PSS MiB/session |")
    out.append("|---|---|---|---|---|---|---|---|---|")
    for c in run["cohorts"]:
        dv = c["derived"]
        out.append("| %s N=%d | %s | %s | %s | %s | %s | %s | %s | %s |" % (
            c["mode"], c["n"], fmt(dv["per_session_total_mib_median"]),
            fmt(dv["per_session_pss_mib_median"]), fmt(dv["per_session_guest_cgroup_peak_mib_median"]),
            fmt(dv["per_session_cli_pss_mib_median"]), fmt(dv["tap_ms_median"], 0),
            fmt(dv["first_frame_ms_median"], 0), fmt(dv["session_wall_s_median"], 2),
            fmt(dv.get("marginal_pss_mib_per_session"))))
    out.append("")
    out.append("| cohort | session | peak PSS | anon | file | shmem | priv dirty | shared | RSS "
               "| threads | fds | gralloc open fds | gralloc alloc MiB | cgroup peak | cpu s | wall s "
               "| launch/first-frame ms | taps ms |")
    out.append("|---|" + "---|" * 18)
    for c in run["cohorts"]:
        for label, v in c["sessions"].items():
            rep = v["reply"] or {}
            out.append("| %s N=%d | %s | %.1f | %.1f | %.1f | %.1f | %.1f | %.1f | %.1f | %d | %d | %d | %.1f | %.1f | %.2f | %.2f | %s | %s |" % (
                c["mode"], c["n"], label, v["peak_pss_mib"], v["pss_anon_peak_mib"], v["pss_file_peak_mib"],
                v["pss_shmem_peak_mib"], v["priv_dirty_peak_mib"], v["shared_peak_mib"],
                v["rss_peak_mib"], v["threads_peak"], v["fds_peak"], v["gralloc_open_fds_peak"],
                v["gralloc_alloc_mib"], v["cgroup"]["peak_current_mib"], v["cpu_s"],
                v["process_wall_s"] or 0.0,
                "%s / %s" % (fmt(rep.get("launch_step_ms"), 0), fmt(rep.get("first_live_frame_ms"), 0)),
                rep.get("tap_ms") or rep.get("counts") or ""))
    out.append("")
    out.append("")
    out.append("| N | Option A window stack (launch − render) | interactive extras (full − launch) |")
    out.append("|---|---|---|")
    for row in run.get("comparison", []):
        oa = row.get("option_a_window_stack") or {}
        ie = row.get("interactive_extras") or {}
        out.append("| %d | PSS %s MiB, cgroup %s MiB (launch cg %s vs render cg %s) | PSS %s MiB, "
                   "cgroup %s MiB (gralloc allocs %s -> %s) |" % (
                       row["n"], fmt(oa.get("pss_delta_mib")), fmt(oa.get("cgroup_peak_delta_mib")),
                       fmt(oa.get("launch_cgroup_peak_mib")), fmt(oa.get("render_cgroup_peak_mib")),
                       fmt(ie.get("pss_delta_mib")), fmt(ie.get("cgroup_peak_delta_mib")),
                       fmt(ie.get("gralloc_alloc_count_launch_median"), 0),
                       fmt(ie.get("gralloc_alloc_count_full_median"), 0)))
    out.append("")
    out.append("")
    out.append("## W29 memory columns (same-type comparison only; BENCHMARKS.md section 11)")
    out.append("")
    out.append("| cohort | cgroup peak MiB/session | cgroup GiB-min total | PSS MiB/session (secondary) "
               "| anon+shmem at exit MiB | anon+shmem settled-60s MiB | legacy ACVM 130.3 MiB "
               "(anon+shmem, warm, 60 s) |")
    out.append("|---|---|---|---|---|---|---|")
    for c in run["cohorts"]:
        mc = (c["derived"].get("memory_columns") or {})
        lc = (mc.get("legacy_comparison") or {})
        verdict = lc.get("verdict") or "NOT_MEASURED"
        why = "; ".join(lc.get("reasons") or [])
        out.append("| %s N=%d | %s | %s | %s | %s | %s | **%s**%s |" % (
            c["mode"], c["n"], fmt(mc.get("cgroup_peak_mib_per_session")),
            fmt(mc.get("cgroup_gib_min_total"), 4),
            fmt(mc.get("pss_mib_per_session_secondary")),
            fmt(mc.get("anon_shmem_at_exit_mib_per_session")),
            fmt(mc.get("anon_shmem_settled_mib_per_session")), verdict,
            (" (" + why + ")") if why else ""))
    out.append("")
    out.append("A row whose metric type or state differs from the legacy row is "
               "NOT_COMPARABLE, and a comparison over a single rep has no measured "
               "spread so it can never PASS (tools/bench/benchlib.py).")
    out.append("")
    out.append("")
    out.append("## W29 repeated-rep statistics (last rep per cohort; one rep = no spread = not passable)")
    out.append("")
    out.append("| cohort | metric | n | p50 | p95 | spread % | 95% CI of p50 | noisy |")
    out.append("|---|---|---|---|---|---|---|---|")
    for c in run["cohorts"]:
        for key, st in (c["derived"].get("rep_stats") or {}).items():
            out.append("| %s N=%d | %s | %s | %s | %s | %s | %s..%s | %s |" % (
                c["mode"], c["n"], key, st.get("n"), fmt(st.get("p50"), 3),
                fmt(st.get("p95"), 3), fmt(st.get("spread_pct")),
                fmt(st.get("ci95_low"), 3), fmt(st.get("ci95_high"), 3),
                "yes" if st.get("noisy") else "no"))
    out.append("")
    out.append("Guest gralloc allocations (`ACOSART_HW_VERBOSE` `allocate:` lines, virtual bytes) "
               "per session: " + ", ".join("%s/%s=%dx%s MiB" % (c["mode"], l, v["gralloc_alloc_count"],
                                                                 fmt(v["gralloc_alloc_mib"]))
                                           for c in run["cohorts"] for l, v in c["sessions"].items()))
    return "\n".join(out) + "\n"


def main():
    idle_host = meminfo()
    print("bench-session: idle baseline %s" % json.dumps(idle_host), file=sys.stderr)
    idle = {"used_mib": round(idle_host["used_mib"], 1),
            "used_pct": round(idle_host["used_pct"], 2),
            "avail_mib": round(idle_host["avail_mib"], 1),
            "pss_mib": round(host_pss_mib(), 1)}
    run = {"schema": "acos-bench-session/1",
           "profile": PROFILE,
           "verdict_eligible": VERDICT_ELIGIBLE,
           "not_verdict_eligible_reason": (None if VERDICT_ELIGIBLE else
                                           "%s build: NOT_VERDICT_ELIGIBLE" % PROFILE),
           "provenance": provenance(),
           "idle_baseline": idle,
           "cohorts": [],
           "refused": []}
    for mode in MODES:
        for n in NS:
            for k in range(1, REPS + 1):
                host = meminfo()
                projected = host["used_mib"] + (350.0 * n)  # worst-case-ish per-session ceiling
                projected_pct = 100.0 * projected / host["total_mib"]
                if host["avail_mib"] < MIN_AVAIL_GIB * 1024.0 or projected_pct > MAX_USED:
                    msg = ("refused %s N=%d rep=%d: avail %.1f GiB, projected %.1f%% > %.0f%%"
                           % (mode, n, k, host["avail_mib"] / 1024.0, projected_pct, MAX_USED))
                    print("bench-session: " + msg, file=sys.stderr)
                    run["refused"].append(msg)
                    continue
                print("bench-session: %s N=%d rep=%d (host used %.1f%%, avail %.1f GiB)"
                      % (mode, n, k, host["used_pct"], host["avail_mib"] / 1024.0), file=sys.stderr)
                repdir = os.path.join(OUT, "session-%s-N%d" % (mode, n), "rep-%d" % k)
                summ = run_cohort(mode, n, repdir, idle)
                summ["rep"] = k
                summ["profile"] = PROFILE
                summ["verdict_eligible"] = VERDICT_ELIGIBLE
                run["cohorts"].append(summ)
                print("bench-session: %s N=%d rep=%d pass %d/%d peak_host %.1f%%"
                      % (mode, n, k, summ["correctness"]["passed"], n,
                         summ["host"]["peak_used_pct"]), file=sys.stderr)
    run["finished_utc"] = datetime.datetime.now(
        datetime.timezone.utc).isoformat(timespec="seconds")
    run["idle_after"] = {"used_mib": round(meminfo()["used_mib"], 1),
                         "pss_mib": round(host_pss_mib(), 1)}
    derive(run)
    with open(os.path.join(OUT, "summary.json"), "w") as f:
        json.dump(run, f, indent=2, sort_keys=True)
    table = render_table(run)
    with open(os.path.join(OUT, "table.md"), "w") as f:
        f.write(table)
    print(table)
    print("summary: %s" % os.path.join(OUT, "summary.json"))


if __name__ == "__main__":
    if len(sys.argv) == 3 and sys.argv[1] == "--analyze":
        _dir = sys.argv[2]
        with open(os.path.join(_dir, "summary.json")) as _f:
            _run = json.load(_f)
        _run["analyzed_utc"] = datetime.datetime.now(
            datetime.timezone.utc).isoformat(timespec="seconds")
        derive(_run)
        with open(os.path.join(_dir, "summary.json"), "w") as _f:
            json.dump(_run, _f, indent=2, sort_keys=True)
        _table = render_table(_run)
        with open(os.path.join(_dir, "table.md"), "w") as _f:
            _f.write(_table)
        print(_table)
        print("summary: %s" % os.path.join(_dir, "summary.json"))
        sys.exit(0)
    main()
PY

# ---- re-analysis only: no ART run, no lock ----
if [ -n "$ANALYZE" ]; then
    [ -f "$ANALYZE/summary.json" ] || { echo "bench-session: no $ANALYZE/summary.json" >&2; exit 2; }
    [ "$ANALYZE/driver.py" -ef "$OUT/driver.py" ] || cp "$OUT/driver.py" "$ANALYZE/driver.py"
    exec python3 -I "$ANALYZE/driver.py" --analyze "$ANALYZE"
fi

MEM="${GUARD_MEM:-}"
if [ -z "$MEM" ]; then
    MAX_N="$(echo "$NS" | tr ' ' '\n' | sort -n | tail -1)"
    if [ "$MAX_N" -ge 20 ]; then MEM="14G"
    elif [ "$MAX_N" -ge 10 ]; then MEM="10G"
    else MEM="8G"; fi
fi

# The whole benchmark holds the host-wide ART lock, so N-session work from other
# agents queues behind it and this run owns the machine for its duration.
exec "$ROOT/tools/art-runtime/run-exclusive.sh" --task "W12-bench" -- \
    "$ROOT/tools/guard/guarded.sh" --mem "$MEM" --time 1800 -- \
    python3 -I "$OUT/driver.py" "$ROOT" "$OUT" "$NS" "$MODES" "$REPS" "$SAMPLE_MS" \
    "$MIN_AVAIL_GIB" "$MAX_USED" "$KEEP_FRAMES" "$PROFILE" "$ACOS" "$ACOS_ART"
