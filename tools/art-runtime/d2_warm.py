#!/usr/bin/env python3
"""d2_warm.py -- W57: the WARM half of the D2 table, on a held `acos serve` ART session.

W39/W52 could only measure `acos verify --engine art` (one cold process per task),
so M1/M2/M5/M6/M7 were NOT_MEASURED or NOT_COMPARABLE purely because of STATE:
legacy measured a warm, held session and ACOS measured a cold-process one-shot.
`acos serve` now holds a real ART session (W50), so this harness measures the
same metrics in the same state, through the daemon's real socket.

Two phases, each driven inside its own `run-exclusive.sh` + `guarded.sh`
acquisition (the host-wide ART lock is never held across reps):

  --phase mem --n 1 --rep K        one held session, 60 s settle after its first
                                   frame -> M1 (MIB anon+shmem) and the N=1 total
  --phase mem --n 5 --rep K        five held sessions, 60 s settle after the last
                                   first frame -> M2 (per-session, incremental and
                                   marginal) and the N=5 total
  --with-latency                   (mem phase only) after the settle, measure on
                                   the SAME held session: M5 fresh screenshot (n)
                                   and M7 tree with text (n), then M6 reset->ready
                                   (n), each with warm-ups discarded
  --analyze OUTDIR                 aggregate the per-rep JSONs into the D2-warm
                                   result (results/bench/d2-warm-<UTC>.json + .md)

Every verdict comes from ``benchlib.verdict()`` (§11.2: spread gate, tie is not a
pass, same metric type AND state or NOT_COMPARABLE); nothing is decided by hand.
Memory is cgroup ``memory.stat`` anon+shmem of the job cgroup (the guarded scope
`acos serve` runs in) plus any guest cgroup outside it, sampled every 100 ms, with
the sampler process's own RssAnon+RssShmem subtracted (W45/W52 rule).

The serve client is `tools/art-runtime/agent_loop.py`'s ``Daemon`` (W55 owns that
file); it is imported, never forked.

Scratch lives under the --outdir (never /tmp) and is pruned after each rep's JSON
unless --keep-scratch.
"""

import argparse
import glob
import json
import os
import shutil
import signal
import subprocess
import sys
import threading
import time
from datetime import datetime, timezone
from pathlib import Path

HERE = Path(__file__).resolve().parent
REPO = HERE.parent.parent
sys.path.insert(0, str(REPO / "tools/bench"))
sys.path.insert(0, str(HERE))

import benchlib  # noqa: E402
import agent_loop  # noqa: E402  (W55-owned; imported, not forked)
sys.path.insert(0, str(REPO / "tools/bench"))
import bench_reply  # noqa: E402  (BENCHFIX: the reply/tree contract)

MiB = 1024.0 * 1024.0
APK = REPO / "fixtures/build/apps/counter.apk"

# Legacy ACVM anchors: BENCHMARKS.md section 3, grounded in
# docs/reference/ACVM_INVENTORY.md section 2 (the inventory defines "workset" as
# the core cgroup's anon + shmem, i.e. the same CGROUP_ANON_SHMEM type this
# harness measures -- checked, not assumed).
LEGACY = {
    "M1": {"value": 206.0, "unit": "MiB", "type": "CGROUP_ANON_SHMEM",
           "state": "warm", "settle_s": 60, "n": 3,
           "source": "BENCHMARKS.md 3 (legacy ACVM W1 diet bare core, anon+shmem "
                     "'workset', settled 60 s)"},
    "M2": {"value": 78.7, "unit": "MiB/session", "type": "CGROUP_ANON_SHMEM",
           "state": "warm", "settle_s": 60, "n": 3,
           "source": "BENCHMARKS.md 3 (legacy v9 per-session N=5, anon+shmem, "
                     "warm, 60 s; inventory: (loaded-bare)/5)"},
    "M5": {"value": 66.5, "unit": "ms", "type": "WALL_MS", "state": "warm",
           "settle_s": None, "n": 3,
           "source": "BENCHMARKS.md 3 (legacy fresh screenshot 0.066-0.067 s p50, "
                     "warm, retrieval latency)"},
    "M6": {"value": 748.1, "unit": "ms", "type": "WALL_MS", "state": "warm",
           "settle_s": None, "n": 12,
           "source": "iter/D2.md via BENCHMARKS.md 3 (legacy reset --app -> first "
                     "fresh frame, 748.1 ms p50, warm; the command alone is 110.3 ms)"},
    "M7": {"value": 538.0, "unit": "ms", "type": "WALL_MS", "state": "warm",
           "settle_s": None, "n": 3,
           "source": "BENCHMARKS.md 3 (legacy one-shot `ui --text` tree 538 ms p50, "
                     "warm; the 4.1 ms persistent route is research, not shipped)"},
}


def utc():
    return datetime.now(timezone.utc).isoformat(timespec="seconds")


def utc_stamp():
    return datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%SZ")


def host_now():
    h = {**benchlib.read_meminfo(), **benchlib.read_loadavg()}
    h["utc"] = utc()
    return h


def host_ok(h):
    return (h.get("loadavg1") is not None and h["loadavg1"] <= 4.0
            and h.get("mem_available_gib") is not None
            and h["mem_available_gib"] >= 30.0)


# ---------------------------------------------------------------- /proc helpers
def anon_shmem_mib(pid):
    """RssAnon + RssShmem of one process in MiB (same type as the cgroup column)."""
    try:
        anon = shmem = None
        for line in Path("/proc/%d/status" % int(pid)).read_text().splitlines():
            if line.startswith("RssAnon:"):
                anon = int(line.split()[1]) / 1024.0
            elif line.startswith("RssShmem:"):
                shmem = int(line.split()[1]) / 1024.0
        if anon is None:
            return None
        return round(anon + (shmem or 0.0), 3)
    except (OSError, ValueError, IndexError):
        return None


def comm_of(pid):
    try:
        return Path("/proc/%d/comm" % int(pid)).read_text().strip()
    except OSError:
        return "?"


def _record_daemon(pid):
    """SERVELEAK2/F2: append `<pid> <starttime>` to `ACOS_BENCH_PIDFILE`.

    `acos-bench.sh` exports the pidfile per run and its reaper signals only the
    pids recorded here, after re-verifying `ACOS_BENCH_RUN_ID` in
    `/proc/<pid>/environ` and the start time (a recycled pid fails). Without the
    file (a bare `d2_warm` run) recording is a no-op, not an error.

    REL3A found this call written but the helper undefined: `NameError` aborted
    the whole latency phase at the first daemon spawn. The implementation is
    SERVELEAK2/F2's (same contract, field 22 read after the last `)`).
    """
    path = os.environ.get("ACOS_BENCH_PIDFILE")
    if not path:
        return
    try:
        stat = Path("/proc/%d/stat" % int(pid)).read_text()
        # Field 22 (starttime); the comm field can hold spaces, so split after
        # the last ')'.
        starttime = stat.rsplit(") ", 1)[1].split()[19]
    except (OSError, IndexError):
        return
    try:
        with open(path, "a") as f:
            f.write("%d %s\n" % (int(pid), starttime))
    except OSError:
        pass


def _environ_has(pid, needle):
    """Does process `pid` carry exactly the `KEY=VALUE` entry `needle`?

    The Python twin of `bench_environ_has` in `tools/bench/reap-daemons.sh`:
    process ownership is a run id in the process's own environment, never a
    command-line or scratch-path match (two runs share both). A caller that
    cannot read the environment (another user's process, a race on exit) is not
    ours.

    REL3A found this call written but the helper undefined (`NameError` aborted
    the latency phase whenever a survivor existed); SERVELEAK2/F2/F1 is the
    implementation, contract unchanged.
    """
    try:
        env = Path("/proc/%d/environ" % int(pid)).read_bytes()
    except OSError:
        return False
    wanted = needle.encode() if isinstance(needle, str) else needle
    return wanted in env.split(b"\0")



def ppid_map():
    """parent -> [child pid] (for descendant walks)."""
    out = {}
    for name in os.listdir("/proc"):
        if not name.isdigit():
            continue
        try:
            stat = Path("/proc/%s/stat" % name).read_text()
            out.setdefault(int(stat.rsplit(")", 1)[1].split()[1]), []).append(int(name))
        except (OSError, ValueError, IndexError):
            continue
    return out


def parent_map():
    """child pid -> parent pid, the shape benchlib.descendant_cgroups expects."""
    out = {}
    for name in os.listdir("/proc"):
        if not name.isdigit():
            continue
        try:
            stat = Path("/proc/%s/stat" % name).read_text()
            out[int(name)] = int(stat.rsplit(")", 1)[1].split()[1])
        except (OSError, ValueError, IndexError):
            continue
    return out


def descendants(root_pid):
    out, stack = set(), [int(root_pid)]
    pm = ppid_map()
    while stack:
        pid = stack.pop()
        if pid in out:
            continue
        out.add(pid)
        stack.extend(pm.get(pid, []))
    return out


def cgroup_anon_shmem(cg):
    """(anon+shmem MiB, memory.current MiB) of one cgroup, or (None, None)."""
    st = benchlib.read_memstat(cg, keys=("anon", "shmem"))
    if st is None:
        return None, None
    cur = benchlib.read_cgroup_int(cg, "memory.current")
    return (round((st.get("anon", 0) + st.get("shmem", 0)) / MiB, 3),
            round(cur / MiB, 3) if cur is not None else None)


class WarmSampler(threading.Thread):
    """100 ms anon+shmem sampler for the job cgroup (+ guests outside it).

    `sampler_pid` is this harness process: it lives in the same guarded scope as
    `acos serve`, so its own RssAnon+RssShmem is charged back out of every sample
    (benchlib's W45/W52 rule). The process-level per-session attribution is taken
    at settle time by the caller from the pid sets it recorded at launch.
    """

    def __init__(self, job, sampler_pid, interval=0.1):
        super().__init__(daemon=True)
        self.job = Path(job)
        self.sampler_pid = int(sampler_pid)
        self.interval = float(interval)
        self.stop_event = threading.Event()
        self.samples = []
        self.extras = []
        self.errors = []
        self.ticks = 0

    def discover(self):
        try:
            found = benchlib.descendant_cgroups(os.getpid(), self.job,
                                                ppid_map=parent_map())
        except (OSError, ValueError) as exc:
            self.errors.append(str(exc))
            found = set()
        self.extras = sorted(str(c) for c in found)

    def sample_once(self):
        self.ticks += 1
        if self.ticks % 10 == 1:
            self.discover()
        job_mib, job_cur = cgroup_anon_shmem(self.job)
        extra = {}
        for cg in self.extras:
            m, _c = cgroup_anon_shmem(cg)
            if m is not None:
                extra[cg] = m
        sampler = anon_shmem_mib(self.sampler_pid) or 0.0
        gross = (job_mib or 0.0) + sum(extra.values())
        self.samples.append({
            "t": time.time(),
            "utc": utc(),
            "job_anon_shmem_mib": job_mib,
            "job_memory_current_mib": job_cur,
            "extra_cgroups": extra,
            "sampler_mib": sampler,
            "net_anon_shmem_mib": round(gross - sampler, 3),
        })
        return self.samples[-1]

    def run(self):
        while not self.stop_event.is_set():
            t0 = time.time()
            try:
                self.sample_once()
            except Exception as exc:  # noqa: BLE001 -- a sampler fault must not fake a row
                self.errors.append("%s: %s" % (type(exc).__name__, exc))
            left = self.interval - (time.time() - t0)
            if left > 0:
                self.stop_event.wait(left)

    def stop(self):
        self.stop_event.set()
        self.join(timeout=5)

    def window(self, t_from, t_to=None):
        rows = [s for s in self.samples
                if s["t"] >= t_from and (t_to is None or s["t"] <= t_to)]
        vals = [s["net_anon_shmem_mib"] for s in rows
                if s.get("net_anon_shmem_mib") is not None]
        if not rows:
            return None
        return {
            "samples": len(rows),
            "net_anon_shmem_mib": benchlib.stats(vals) if vals else None,
            "last": rows[-1],
            "first": rows[0],
        }


# ---------------------------------------------------------------- serve control
class Serve:
    def __init__(self, acos, scratch, engine="art", max_sessions=8, idle=900,
                 extra_env=None):
        self.scratch = Path(scratch)
        self.socket_path = str(self.scratch / "serve.sock")
        self.stdout = open(self.scratch / "serve.stdout", "w")
        self.stderr = open(self.scratch / "serve.log", "w")
        env = dict(os.environ,
                   ACOS_RUNTIME_DIR=str(self.scratch / "runtime"),
                   ACOS_CACHE_DIR=str(self.scratch / "cache"),
                   ACOS_SERVE_ARTIFACTS=str(self.scratch / "sessions"),
                   # A harness may need the product's own instrumentation (the
                   # APK-ready stage ladder, ACOS_ART_STAGE_TIMING). It inherits
                   # to the launcher children, never set globally.
                   **(extra_env or {}))
        # SERVELEAK: `--owner-pid` ties this rep's daemon to the harness. If the
        # harness dies hard (kill -9, a killed guard), the daemon destroys its
        # guest sessions and exits instead of holding live ART guests for
        # `idle` seconds. The daemon is nobody else's: it is spawned here, with
        # this rep's socket under this rep's scratch.
        self.argv = [acos, "serve", "--socket", self.socket_path,
                     "--engine", engine, "--idle-timeout", str(idle),
                     "--max-sessions", str(max_sessions),
                     "--owner-pid", str(os.getpid())]
        self.proc = subprocess.Popen(self.argv, cwd=str(REPO), env=env,
                                     stdin=subprocess.DEVNULL,
                                     stdout=self.stdout, stderr=self.stderr,
                                     start_new_session=True)
        # SERVELEAK2/F2: record this daemon (pid + start time) for the run's
        # reaper. `acos-bench.sh` sets ACOS_BENCH_PIDFILE per run and exports
        # ACOS_BENCH_RUN_ID, which this daemon inherits through `env`; the reaper
        # re-verifies both before it signals anything, so a concurrent bench run
        # sharing the scratch root can never reap this daemon (or be reaped by
        # it).
        _record_daemon(self.proc.pid)
        deadline = time.time() + 30
        while time.time() < deadline:
            if Path(self.socket_path).exists():
                return
            if self.proc.poll() is not None:
                raise RuntimeError("acos serve died before binding (rc=%s); see %s"
                                   % (self.proc.returncode, self.stderr.name))
            time.sleep(0.05)
        raise RuntimeError("acos serve never bound %s" % self.socket_path)

    def client(self):
        return agent_loop.Daemon(self.socket_path, timeout=180.0)

    def close(self):
        """Stop the daemon and prove it is gone (SERVELEAK).

        `acos serve` destroys its guest sessions on SIGTERM before it exits, so a
        terminated daemon cannot leave a bwrap/dalvikvm64 behind. The daemon is
        started with `start_new_session=True`, so the group kill below can only
        reach processes this rep created — never another agent's.
        """
        try:
            if self.proc.poll() is None:
                self.proc.terminate()
                try:
                    self.proc.wait(timeout=20)
                except subprocess.TimeoutExpired:
                    # SERVELEAK2/F4: signal the group **while the leader is still
                    # unreaped**. Its pid (hence its group id) cannot be recycled
                    # until it is waited for, so `killpg` here names this rep's
                    # own group and never a stranger's. A `killpg` after `wait()`
                    # is the hazard the review found: the id is free by then and
                    # another agent's `setsid`/`bwrap` can own it.
                    try:
                        os.killpg(self.proc.pid, signal.SIGKILL)
                    except OSError:
                        pass
                    self.proc.kill()
                    self.proc.wait(timeout=10)
        finally:
            for fh in (self.stdout, self.stderr):
                try:
                    fh.close()
                except OSError:
                    pass
        return self.proc.returncode


def survivors(scratch):
    """Processes still carrying this rep's scratch path (a leak, never ignored)."""
    out = []
    for name in os.listdir("/proc"):
        if not name.isdigit():
            continue
        try:
            cmd = Path("/proc/%s/cmdline" % name).read_bytes().replace(b"\0", b" ").decode(
                "utf-8", "replace")
        except OSError:
            continue
        if str(scratch) in cmd and int(name) != os.getpid():
            # SERVELEAK2/F2/F1: a name match is not ownership. Two runs sharing
            # the default scratch root name the same paths, so a report must be
            # filtered by the run id every process this run started inherits
            # (`acos-bench.sh` exports it; the daemon passes it on). A process
            # without our run id belongs to another run: not a leak of ours.
            run_id = os.environ.get("ACOS_BENCH_RUN_ID")
            if run_id and not _environ_has(int(name), "ACOS_BENCH_RUN_ID=" + run_id):
                continue
            out.append({"pid": int(name), "comm": comm_of(int(name)), "cmd": cmd[:240]})
    return out


# ---------------------------------------------------------------- latency verbs
PNG_MAGIC = b"\x89PNG\r\n\x1a\n"


def measure_screenshot(daemon, base, count, warmup=0):
    """M5: a fresh screenshot request on the already-held session.

    BENCHFIX: FB2c confines a screenshot destination to the session's artifacts
    directory, so the old `screenshot: <scratch>/m5-000.png` is an argument error
    and the reply carried no `screenshot` block at all (M5 scored zero good
    rows). `screenshot: true` is the documented agent form: the daemon writes the
    PNG under the session's own artifacts and reports `screenshot.path`; that
    reported file is what this measures, never a caller-invented path.
    """
    rows = []
    for i in range(warmup + count):
        where = "m5-%03d" % i
        r = daemon.call("acos_observe", {"base": base, "tree": False, "text": False,
                                         "screenshot": True})
        env = r["envelope"]
        value = env.get("result") or {}
        frame = value.get("frame") or {}
        path = None
        size = 0
        magic = False
        error_detail = None
        error = (env.get("error") or {}).get("code") if env.get("error") else None
        if env.get("ok"):
            try:
                path, _meta = bench_reply.screenshot_path(value, where)
            except bench_reply.ReplyShapeError as exc:
                error = "SCREENSHOT_SHAPE"
                error_detail = str(exc)
            else:
                size = path.stat().st_size if path.exists() else 0
                if size:
                    try:
                        with open(path, "rb") as fh:
                            magic = fh.read(8) == PNG_MAGIC
                    except OSError:
                        magic = False
        seq, prev = frame.get("frame_seq"), frame.get("previous_seq")
        fresh = (isinstance(seq, int) and isinstance(prev, int) and seq > prev)
        row = {
            "tag": where, "warmup": i < warmup,
            "ms": round(r["wall_ms"], 3), "ok": bool(env.get("ok")),
            "frame_seq": seq, "previous_seq": prev, "fresh": fresh,
            "meaningful": bool(frame.get("meaningful")), "png_bytes": size,
            "png_magic": magic, "response_bytes": r["response_bytes"],
            "screenshot_path": str(path) if path else None,
            "error": error, "error_detail": error_detail,
        }
        row["good"] = bool(row["ok"] and row["meaningful"] and size > 0
                           and magic and fresh)
        rows.append(row)
        try:
            if path is not None:
                path.unlink()
        except OSError:
            pass
    return rows


def count_text(tree):
    node = agent_loop.find_node(tree, "count")
    if node is None:
        return None
    return agent_loop.node_text(node).strip()


def text_nodes(tree):
    return sum(1 for n in agent_loop.walk((tree or {}).get("nodes"))
               if agent_loop.node_text(n).strip())


def measure_tree(daemon, base, count, warmup=0):
    """M7: one tree dump with text on the already-held session."""
    rows = []
    for i in range(warmup + count):
        r = daemon.call("acos_observe_tree", {"base": base, "max_nodes": 2000})
        env = r["envelope"]
        value = env.get("result") or {}
        tree = value.get("tree") if isinstance(value.get("tree"), dict) else {}
        nodes = sum(1 for _ in agent_loop.walk(tree.get("nodes")))
        texts = text_nodes(tree)
        tree_bytes = len(json.dumps(tree, separators=(",", ":")).encode()) if tree else 0
        rows.append({
            "tag": "m7-%03d" % i, "warmup": i < warmup,
            "ms": round(r["wall_ms"], 3), "ok": bool(env.get("ok")),
            "nodes": nodes, "text_nodes": texts, "count_text": count_text(tree),
            "tree_bytes": tree_bytes, "truncated": bool(value.get("truncated")),
            "response_bytes": r["response_bytes"],
            "error": (env.get("error") or {}).get("code") if env.get("error") else None,
        })
        rows[-1]["good"] = bool(rows[-1]["ok"] and nodes > 0 and texts > 0
                                and not rows[-1]["truncated"])
    return rows


def measure_reset(daemon, base, count, warmup=0):
    """M6: `acos_reset` -> the next correct frame.

    The reset reply is only sent after the guest was stopped, the same APK
    relaunched with empty app-data and a fresh frame bound (serve.rs `reset`),
    so the request wall time IS reset -> ready. Readiness is proved, not assumed:
    the reply must report a newer `frame_seq` than `previous_frame_seq`, and a
    following tree read at that frame must show the counter back at 0 after the
    app was tapped to a non-zero value.
    """
    rows = []
    for i in range(warmup + count):
        setup = {"tap_ok": None, "before_text": None, "verify_ms": None,
                 "after_text": None, "verify_frame_seq": None}
        tap = daemon.call("acos_tap", {"base": base, "id": "inc"})
        setup["tap_ok"] = bool(tap["envelope"].get("ok"))
        before = daemon.call("acos_observe_tree", {"base": base, "max_nodes": 2000})
        setup["before_text"] = count_text((before["envelope"].get("result") or {})
                                          .get("tree"))
        r = daemon.call("acos_reset", {"base": base})
        env = r["envelope"]
        value = env.get("result") or {}
        verify = daemon.call("acos_observe_tree", {"base": base, "max_nodes": 2000})
        vtree = (verify["envelope"].get("result") or {}).get("tree")
        setup["verify_ms"] = round(verify["wall_ms"], 3)
        setup["after_text"] = count_text(vtree)
        setup["verify_frame_seq"] = (verify["envelope"].get("result") or {}).get(
            "frame", {}).get("frame_seq") if isinstance(
                (verify["envelope"].get("result") or {}).get("frame"), dict) else None
        seq, prev = value.get("frame_seq"), value.get("previous_frame_seq")
        rows.append({
            "tag": "m6-%03d" % i, "warmup": i < warmup,
            "ms": round(r["wall_ms"], 3), "ok": bool(env.get("ok")),
            "state_reset": bool(value.get("state_reset")),
            "data_cleared": bool(value.get("data_cleared")),
            "frame_seq": seq, "previous_frame_seq": prev,
            "fresh": (isinstance(seq, int) and isinstance(prev, int) and seq > prev),
            "setup": setup,
            "response_bytes": r["response_bytes"],
            "error": (env.get("error") or {}).get("code") if env.get("error") else None,
        })
        rows[-1]["good"] = bool(
            rows[-1]["ok"] and rows[-1]["state_reset"] and rows[-1]["data_cleared"]
            and rows[-1]["fresh"]
            and setup["before_text"] not in (None, "0")
            and setup["after_text"] == "0")
    return rows


# ---------------------------------------------------------------- phases
def launch_session(daemon, index, tag):
    session_id = "s_w57_%s_%d" % (tag, index)
    return session_id, {"request_id": "w57-%s-%d" % (tag, index),
                        "session_id": session_id}


def run_mem_phase(args, outdir):
    tag = "n%d-rep%d" % (args.n, args.rep)
    scratch = Path(outdir) / ("scratch-%s" % tag)
    if scratch.exists():
        shutil.rmtree(scratch)
    (scratch / "screens").mkdir(parents=True)
    doc = {"schema": "acos-bench-d2-warm-rep/1", "utc": utc(), "phase": "mem",
           "n": args.n, "rep": args.rep, "settle_s": args.settle,
           "binary": args.acos, "apk": str(APK),
           "apk_sha256": benchlib.sha256_file(APK),
           "host_before": host_now(), "sessions": [], "ok": False}
    serve = None
    daemon = None
    sampler = None
    t0 = time.time()
    try:
        probe = benchlib.binary_provenance(args.acos)
        doc["profile"] = probe.get("profile")
        doc["binary_sha256"] = probe.get("sha256")
        doc["verdict_eligible_binary"] = probe.get("verdict_eligible")
        if not probe.get("verdict_eligible"):
            raise RuntimeError("refusing a non-release binary: %s"
                               % probe.get("not_eligible_reason"))
        serve = Serve(args.acos, scratch)
        job = benchlib.cgroup_path(serve.proc.pid)
        if job is None:
            raise RuntimeError("cannot resolve the job cgroup of acos serve pid %d"
                               % serve.proc.pid)
        doc["job_cgroup"] = str(job)
        daemon = serve.client()
        ping = daemon.call("ping", {})
        doc["ping"] = {"ms": round(ping["wall_ms"], 3),
                       "result": ping["envelope"].get("result")}
        sampler = benchlib.ChurnSampler(
            job, sampler_pid=os.getpid(), root_pid=os.getpid(),
            scope_parent=Path(job).parent,
            artifacts=[str(scratch / "sessions")])
        sampler.start()
        # The daemon-only baseline ("bare" for the incremental formula): no
        # session, so no booted framework -- stated as such wherever it is used.
        time.sleep(args.bare_settle)
        bare = sampler.window(time.time() - args.bare_settle + 1.0)
        doc["bare"] = {"settle_s": args.bare_settle,
                       "net_anon_shmem_mib": (bare["last"]["net_anon_shmem_mib"]
                                              if bare and bare.get("last") else None),
                       "stats": bare["net_anon_shmem_mib"] if bare else None}

        sessions = []
        for i in range(args.n):
            before_pids = descendants(serve.proc.pid)
            session_id, base = launch_session(daemon, i, tag)
            r = daemon.call("acos_launch", {"base": base, "apk": str(APK)})
            env = r["envelope"]
            value = env.get("result") or {}
            new_pids = sorted(descendants(serve.proc.pid) - before_pids)
            frame = value.get("frame") if isinstance(value.get("frame"), dict) else {}
            rec = {"i": i, "session_id": session_id, "launch_ms": round(r["wall_ms"], 3),
                   "ok": bool(env.get("ok")),
                   "first_frame_at": time.time() if env.get("ok") else None,
                   "frame_seq": frame.get("frame_seq"),
                   "pid_count": len(new_pids),
                   "procs": [{"pid": p, "comm": comm_of(p)} for p in new_pids],
                   "error": (env.get("error") or {}).get("code")
                   if env.get("error") else None,
                   "tree_node_count": value.get("tree_node_count")}
            sessions.append(rec)
            if not env.get("ok"):
                break
        doc["sessions"] = sessions
        firsts = [s["first_frame_at"] for s in sessions if s.get("first_frame_at")]
        if len(firsts) != args.n:
            raise RuntimeError("%d/%d sessions reached a first frame"
                               % (len(firsts), args.n))
        t_last = max(firsts)
        doc["last_first_frame_utc"] = utc()
        # Settle: 60 s after the LAST first frame, sampled every 100 ms.
        deadline = t_last + args.settle
        while time.time() < deadline:
            time.sleep(min(1.0, max(0.0, deadline - time.time())))
        settled_from = deadline - 5.0
        win = sampler.window(settled_from)
        doc["settled"] = {
            "settle_s": args.settle,
            "window_s": 5.0,
            "net_anon_shmem_mib": win["net_anon_shmem_mib"] if win else None,
            "last": win["last"] if win else None,
            "samples": win["samples"] if win else 0,
        }
        total_mib = ((win["last"]["net_anon_shmem_mib"])
                     if win and win.get("last") else None)
        per_session = []
        for s in sessions:
            alive = [p for p in s["procs"]
                     if Path("/proc/%d" % p["pid"]).exists()]
            vals = [anon_shmem_mib(p["pid"]) for p in alive]
            vals = [v for v in vals if v is not None]
            per_session.append({
                "i": s["i"],
                "procs": [{"pid": p["pid"], "comm": p["comm"],
                           "anon_shmem_mib": anon_shmem_mib(p["pid"])} for p in alive],
                "process_anon_shmem_mib": round(sum(vals), 3) if vals else None,
            })
        doc["per_session"] = per_session
        doc["total_anon_shmem_mib"] = total_mib
        bare_mib = doc["bare"]["net_anon_shmem_mib"]
        doc["incremental_mib_per_session"] = (
            round((total_mib - bare_mib) / args.n, 3)
            if total_mib is not None and bare_mib is not None else None)

        if args.with_latency:
            base = {"request_id": "w57-lat", "session_id": sessions[0]["session_id"]}
            lat = {"utc": utc(), "n": args.samples, "resets": args.resets,
                   "settle_s": args.settle, "tag": tag,
                   "host_before": host_now()}
            lat["m5"] = measure_screenshot(daemon, base, args.samples,
                                           warmup=args.warmup)
            lat["m7"] = measure_tree(daemon, base, args.samples, warmup=args.warmup)
            lat["m6"] = measure_reset(daemon, base, args.resets, warmup=args.warmup)
            lat["host_after"] = host_now()
            lat["contaminated"] = not (host_ok(lat["host_before"])
                                       and host_ok(lat["host_after"]))
            doc["latency"] = lat
            with open(Path(outdir) / ("latency-%s.json" % tag), "w") as f:
                json.dump(lat, f, indent=2)

        doc["ok"] = True
    except Exception as exc:  # noqa: BLE001 -- record the failure, never fake a row
        doc["error"] = "%s: %s" % (type(exc).__name__, exc)
    finally:
        if sampler is not None:
            sampler.stop()
            doc["sample_count"] = len(sampler.samples)
            doc["sampler_errors"] = sampler.errors[:5]
            doc["sampler_summary"] = sampler.summary()
            with open(scratch / "samples.json", "w") as f:
                json.dump(sampler.samples, f)
        if daemon is not None:
            try:
                for s in doc.get("sessions") or []:
                    daemon.call("acos_destroy", {"base": {
                        "request_id": "w57-destroy-%s" % s["session_id"],
                        "session_id": s["session_id"]}})
            except Exception:  # noqa: BLE001
                pass
            try:
                daemon.close()
            except Exception:  # noqa: BLE001
                pass
        if serve is not None:
            doc["serve_rc"] = serve.close()
        time.sleep(0.5)
        doc["survivors"] = survivors(scratch)
        doc["host_after"] = host_now()
        doc["contaminated"] = not (host_ok(doc["host_before"])
                                   and host_ok(doc["host_after"]))
        doc["wall_s"] = round(time.time() - t0, 3)
        doc["utc_finished"] = utc()
        path = Path(outdir) / ("mem-n%d-rep%d.json" % (args.n, args.rep))
        with open(path, "w") as f:
            json.dump(doc, f, indent=2)
        print("d2_warm: wrote %s ok=%s total=%s incremental=%s survivors=%d"
              % (path, doc.get("ok"), doc.get("total_anon_shmem_mib"),
                 doc.get("incremental_mib_per_session"), len(doc.get("survivors") or [])))
        if not args.keep_scratch:
            shutil.rmtree(scratch, ignore_errors=True)
    return doc


def run_latency_phase(args, outdir):
    tag = "lat-rep%d" % args.rep
    scratch = Path(outdir) / ("scratch-%s" % tag)
    if scratch.exists():
        shutil.rmtree(scratch)
    (scratch / "screens").mkdir(parents=True)
    doc = {"schema": "acos-bench-d2-warm-rep/1", "utc": utc(), "phase": "latency",
           "n": args.n, "rep": args.rep, "binary": args.acos,
           "apk_sha256": benchlib.sha256_file(APK), "host_before": host_now(),
           "ok": False}
    serve = None
    daemon = None
    try:
        serve = Serve(args.acos, scratch)
        daemon = serve.client()
        daemon.call("ping", {})
        base = {"request_id": "w57-lat", "session_id": "s_w57_lat"}
        launch = daemon.call("acos_launch", {"base": base, "apk": str(APK)})
        if not launch["envelope"].get("ok"):
            raise RuntimeError("launch failed: %s" % (launch["envelope"].get("error"),))
        time.sleep(args.settle_short)
        doc["m5"] = measure_screenshot(daemon, base, args.samples,
                                       warmup=args.warmup)
        doc["m7"] = measure_tree(daemon, base, args.samples, warmup=args.warmup)
        doc["m6"] = measure_reset(daemon, base, args.resets, warmup=args.warmup)
        doc["ok"] = True
    except Exception as exc:  # noqa: BLE001
        doc["error"] = "%s: %s" % (type(exc).__name__, exc)
    finally:
        if daemon is not None:
            try:
                daemon.call("acos_destroy", {"base": {
                    "request_id": "w57-destroy-lat", "session_id": "s_w57_lat"}})
            except Exception:  # noqa: BLE001
                pass
            try:
                daemon.close()
            except Exception:  # noqa: BLE001
                pass
        if serve is not None:
            doc["serve_rc"] = serve.close()
        time.sleep(0.5)
        doc["survivors"] = survivors(scratch)
        doc["host_after"] = host_now()
        doc["contaminated"] = not (host_ok(doc["host_before"])
                                   and host_ok(doc["host_after"]))
        doc["utc_finished"] = utc()
        path = Path(outdir) / ("latency-%s.json" % tag)
        with open(path, "w") as f:
            json.dump(doc, f, indent=2)
        print("d2_warm: wrote %s ok=%s" % (path, doc.get("ok")))
        if not args.keep_scratch:
            shutil.rmtree(scratch, ignore_errors=True)
    return doc


# ---------------------------------------------------------------- analysis
def legacy_stats(mid, spread=None):
    lg = LEGACY[mid]
    return {"n": lg["n"], "p50": lg["value"], "median": lg["value"],
            "p5": None, "p95": None,
            "spread": (float(spread) if spread is not None else 0.0),
            "spread_pct": (100.0 * float(spread) if spread is not None else 0.0),
            "spread_known": spread is not None}


def decide(mid, acos_stats, *, state, extra_reasons=(), contaminated=0):
    lg = LEGACY[mid]
    v = benchlib.verdict(acos_stats, legacy_stats(mid), metric_type=lg["type"],
                         legacy_type=lg["type"], state=state,
                         legacy_state=lg["state"], min_samples=3)
    for r in extra_reasons:
        if r not in v["reasons"]:
            v["reasons"].append(r)
    if contaminated:
        v["reasons"].append("%d rep(s) recorded a failed host gate (loadavg1 > 4 "
                            "or MemAvailable < 30 GiB) and are excluded from the "
                            "verdict" % contaminated)
        if v["verdict"] == "PASS":
            v["verdict"] = "NOISY"
            v["pass"] = False
            v["reasons"].append("a contaminated run can never PASS")
    return v


def make_row(mid, name, unit, state, verdict, value, stats, detail, n,
             extra=None, contaminated=0, settle_s=None):
    lg = LEGACY[mid]
    r = {
        "id": mid, "metric": name, "unit": unit,
        "acos": {"value": value, "metric_type": benchlib.canonical_metric_type(lg["type"]),
                 "state": state, "n": n, "stats": stats, "detail": detail,
                 "settle_s": settle_s},
        "legacy": {"value": lg["value"], "unit": lg["unit"],
                   "metric_type": benchlib.canonical_metric_type(lg["type"]),
                   "state": lg["state"], "source": lg["source"],
                   "settle_s": lg["settle_s"],
                   "spread_known": False},
        "verdict": verdict["verdict"], "pass": bool(verdict["pass"]),
        "ratio": verdict.get("ratio"), "noise": verdict.get("noise"),
        "threshold": verdict.get("threshold"),
        "reasons": verdict["reasons"],
        "contaminated_samples": contaminated,
    }
    st = stats or {}
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


def _good(rows):
    return [r for r in rows if not r.get("warmup") and r.get("good")]


def _failed(rows):
    return [r for r in rows if not r.get("warmup") and not r.get("good")]


def build_rows(mem_docs, lat_docs):
    mem_docs = [d for d in mem_docs if d.get("ok")]
    n1 = [d for d in mem_docs if d.get("n") == 1]
    n5 = [d for d in mem_docs if d.get("n") == 5]
    rows = []

    # ---- M1: fixed idle core, anon+shmem, warm, 60 s settle ----
    m1_vals = [d["total_anon_shmem_mib"] for d in n1
               if not d.get("contaminated")
               and d.get("total_anon_shmem_mib") is not None]
    m1_st = benchlib.stats(m1_vals) if m1_vals else None
    m1_cont = sum(1 for d in n1 if d.get("contaminated"))
    bare_vals = [d["bare"]["net_anon_shmem_mib"] for d in n1
                 if (d.get("bare") or {}).get("net_anon_shmem_mib") is not None]
    sess_vals = []
    for d in n1:
        for s in d.get("per_session") or []:
            if s.get("process_anon_shmem_mib") is not None:
                sess_vals.append(s["process_anon_shmem_mib"])
    m1_extra = {}
    if m1_st and bare_vals:
        bst = benchlib.stats(bare_vals)
        m1_extra = {
            "daemon_only_anon_shmem_mib": bst,
            "guest_process_anon_shmem_mib": (benchlib.stats(sess_vals)
                                             if sess_vals else None),
            "derived_legacy_core_plus_one_session_mib": 206.0 + 130.3,
            "derived_comparison_note": (
                "206.0 + 130.3 = 336.3 MiB is a DERIVED legacy figure "
                "(BENCHMARKS 3 core + per-session N=1), not a published row: it is "
                "information only and never a verdict input."),
        }
    m1_state = "warm"
    m1_verdict = decide(
        "M1", m1_st, state=m1_state, contaminated=m1_cont,
        extra_reasons=(
            "Measured on the ACOS analogue of an idle core: `acos serve` plus one "
            "launched, untouched counter session, settled 60 s after its first "
            "frame (reads only). Legacy 206.0 MiB is a bare core with NO app "
            "session, so this row charges ACOS for one idle session the legacy "
            "number does not carry (see daemon_only and per-session components). "
            "Type checked: the inventory defines legacy 'workset' as the core "
            "cgroup's anon+shmem, the same CGROUP_ANON_SHMEM column measured here.",
        ))
    rows.append(make_row(
        "M1", "Fixed idle core memory (held ART session, no interaction, 60 s settle)",
        "MiB", m1_state, m1_verdict, m1_st["p50"] if m1_st else None, m1_st,
        "Σ cgroup anon+shmem of the job cgroup (+ guests outside it) 60 s after the "
        "first frame, sampler's own RssAnon+RssShmem subtracted; n=1 rep per "
        "acquisition", m1_st["n"] if m1_st else 0,
        extra={**m1_extra, "compare_memory": benchlib.compare_memory(
            {"value_mib": (m1_st["p50"] if m1_st else None),
             "metric_type": "CGROUP_ANON_SHMEM", "state": "warm", "settle_s": 60,
             "n": m1_st["n"] if m1_st else 0},
            benchlib.legacy_memory_reference(206.0))},
        contaminated=m1_cont, settle_s=60))

    # ---- M2: per-session memory at N=5, anon+shmem, warm, 60 s ----
    m2_vals = [d["incremental_mib_per_session"] for d in n5
               if not d.get("contaminated")
               and d.get("incremental_mib_per_session") is not None]
    m2_st = benchlib.stats(m2_vals) if m2_vals else None
    m2_cont = sum(1 for d in n5 if d.get("contaminated"))
    tot1 = [d["total_anon_shmem_mib"] for d in n1
            if not d.get("contaminated") and d.get("total_anon_shmem_mib") is not None]
    tot5 = [d["total_anon_shmem_mib"] for d in n5
            if not d.get("contaminated") and d.get("total_anon_shmem_mib") is not None]
    marginal = None
    marginal_detail = None
    if tot1 and tot5:
        marginal = ((sum(tot5) / len(tot5)) - (sum(tot1) / len(tot1))) / 4.0
        marginal_detail = {
            "n1_total_mib": benchlib.stats(tot1),
            "n5_total_mib": benchlib.stats(tot5),
            "formula": "(mean total(N=5) - mean total(N=1)) / 4, both settled 60 s "
                       "after the last first frame",
            "marginal_mib_per_session": round(marginal, 3),
        }
    m2_verdict = decide(
        "M2", m2_st, state="warm", contaminated=m2_cont,
        extra_reasons=(
            "Value is the legacy formula (loaded - bare)/5 with `bare` = the "
            "`acos serve` daemon alone, settled 10 s before any launch (ACOS has "
            "no booted framework without a session, so the bare baseline is the "
            "daemon, not a booted core -- stated, not hidden). Marginal "
            "((N=5 - N=1)/4) is reported beside it. %d clean N=5 rep(s) remain "
            "after excluding failed and host-gate-contaminated attempts (all of them "
            "are kept in the raw record)." % (m2_st["n"] if m2_st else 0),))
    rows.append(make_row(
        "M2", "Per-session memory at N=5 (incremental and marginal)", "MiB/session",
        "warm", m2_verdict, m2_st["p50"] if m2_st else None, m2_st,
        "5 held sessions, settled 60 s after the last first frame; incremental = "
        "(total - daemon_only)/5 per rep; marginal from the N=1 and N=5 totals",
        m2_st["n"] if m2_st else 0,
        extra={"marginal": marginal_detail,
               "per_session_process_anon_shmem_mib": [
                   s for d in n5 for s in (d.get("per_session") or [])] or None,
               "compare_memory": benchlib.compare_memory(
                   {"value_mib": (m2_st["p50"] if m2_st else None),
                    "metric_type": "CGROUP_ANON_SHMEM", "state": "warm",
                    "settle_s": 60, "n": m2_st["n"] if m2_st else 0},
                   benchlib.legacy_memory_reference(78.7))},
        contaminated=m2_cont, settle_s=60))

    # ---- latency rows ----
    m5 = [r for d in lat_docs for r in _good(d.get("m5") or [])]
    m5_bad = [r for d in lat_docs for r in _failed(d.get("m5") or [])]
    m7 = [r for d in lat_docs for r in _good(d.get("m7") or [])]
    m7_bad = [r for d in lat_docs for r in _failed(d.get("m7") or [])]
    m6 = [r for d in lat_docs for r in _good(d.get("m6") or [])]
    m6_bad = [r for d in lat_docs for r in _failed(d.get("m6") or [])]
    lat_cont = sum(1 for d in lat_docs if d.get("contaminated"))
    m5_st = benchlib.stats([r["ms"] for r in m5]) if m5 else None
    m7_st = benchlib.stats([r["ms"] for r in m7]) if m7 else None
    m6_st = benchlib.stats([r["ms"] for r in m6]) if m6 else None
    rows.append(make_row(
        "M5", "Fresh screenshot retrieval (warm, held session)", "ms", "warm",
        decide("M5", m5_st, state="warm", contaminated=lat_cont,
               extra_reasons=(
                   "One `acos_observe {png:true, screenshot:path}` request on the "
                   "already-held session, timed from request write to reply: host "
                   "capture + PNG write to the requested path (the shipped verb). "
                   "Freshness is part of the sample: frame_seq > previous_seq, PNG "
                   "magic and non-zero bytes, else the sample is dropped. "
                   "%d/%d ok." % (len(m5), len(m5) + len(m5_bad)),)),
        m5_st["p50"] if m5_st else None, m5_st,
        "request-sequenced fresh screenshot on a held session; warm-ups discarded",
        m5_st["n"] if m5_st else 0,
        extra={"failed_samples": m5_bad, "png_bytes": benchlib.stats(
            [r["png_bytes"] for r in m5 if r.get("png_bytes") is not None])},
        contaminated=lat_cont))
    rows.append(make_row(
        "M6", "Reset -> ready (warm, held session)", "ms", "warm",
        decide("M6", m6_st, state="warm", contaminated=lat_cont,
               extra_reasons=(
                   "serve's `acos_reset` (kill the guest, relaunch the same APK "
                   "with empty app-data, bind a fresh frame) timed request-write to "
                   "reply; the reply is only sent once the fresh frame is bound, so "
                   "this IS reset -> ready. Readiness + correctness bound per "
                   "sample: frame_seq > previous_frame_seq, the app was tapped to a "
                   "non-zero count first, and the tree after the reset reads 0 "
                   "(%d/%d ok). Legacy's reset-command-only value (110.3 ms) is "
                   "reported in the reason only, never substituted."
                   % (len(m6), len(m6) + len(m6_bad)),)),
        m6_st["p50"] if m6_st else None, m6_st,
        "reset request wall on a held session, readiness proved by the reply's "
        "fresh frame and a post-reset tree read", m6_st["n"] if m6_st else 0,
        extra={"legacy_command_only_ms": 110.3, "failed_samples": m6_bad},
        contaminated=lat_cont))
    rows.append(make_row(
        "M7", "UI tree with text (warm, held session)", "ms", "warm",
        decide("M7", m7_st, state="warm", contaminated=lat_cont,
               extra_reasons=(
                   "One `acos_observe_tree` request on the already-held session: "
                   "guest traversal + serialization, text included in every node. "
                   "Legacy's comparable row is the SHIPPED one-shot `ui --text` "
                   "tree (538 ms p50 warm); the 4.1 ms persistent-observe number is "
                   "research, not a shipped route, so it is not the bar. "
                   "%d/%d ok." % (len(m7), len(m7) + len(m7_bad)),)),
        m7_st["p50"] if m7_st else None, m7_st,
        "tree request on a held session with text nodes present (not truncated)",
        m7_st["n"] if m7_st else 0,
        extra={"failed_samples": m7_bad,
               "tree_bytes": benchlib.stats(
                   [r["tree_bytes"] for r in m7 if r.get("tree_bytes") is not None]),
               "text_nodes": benchlib.stats(
                   [r["text_nodes"] for r in m7 if r.get("text_nodes") is not None])},
        contaminated=lat_cont))
    order = {"M1": 1, "M2": 2, "M5": 5, "M6": 6, "M7": 7}
    rows.sort(key=lambda r: order[r["id"]])
    return rows


def analyze(outdir, out_json=None, markdown=None):
    outdir = Path(outdir)
    mem_docs = []
    for p in sorted(glob.glob(str(outdir / "mem-n*-rep*.json"))):
        try:
            with open(p) as f:
                d = json.load(f)
            d["_path"] = p
            mem_docs.append(d)
        except (OSError, ValueError):
            continue
    lat_docs = []
    for p in sorted(glob.glob(str(outdir / "latency-*.json"))):
        try:
            with open(p) as f:
                d = json.load(f)
            d["_path"] = p
            lat_docs.append(d)
        except (OSError, ValueError):
            continue
    rows = build_rows(mem_docs, lat_docs)
    counts = {}
    for r in rows:
        counts[r["verdict"]] = counts.get(r["verdict"], 0) + 1
    binaries = []
    for d in mem_docs + lat_docs:
        if d.get("binary") and d["binary"] not in binaries:
            binaries.append(d["binary"])
    acos_art = str(Path(binaries[0]).with_name("acos-art")) if binaries else None
    prov = None
    if binaries and os.path.exists(binaries[0]) and acos_art and os.path.exists(acos_art):
        prov = benchlib.provenance(
            REPO, [binaries[0], acos_art], profile="release", engine="art",
            gate=benchlib.host_gate(str(REPO), check_lock=False),
            extra={"method": "BENCHMARKS.md 11/12; warm rows on a held `acos serve` "
                             "ART session; one ART run per run-exclusive.sh acquisition",
                   "harness": "tools/art-runtime/d2_warm.py"})
    doc = {
        "schema": "acos-bench-d2-warm/1",
        "utc": utc(),
        "profile": "release",
        "verdict_eligible": bool(prov and prov.get("verdict_eligible")),
        "engine": "art",
        "transport": "acos serve unix socket, newline JSON (agent_loop.Daemon)",
        "settings": {"settle_s": (mem_docs[0].get("settle_s") if mem_docs else None),
                     "m1_reps": len([d for d in mem_docs if d.get("n") == 1]),
                     "m2_reps": len([d for d in mem_docs if d.get("n") == 5]),
                     "latency_docs": len(lat_docs),
                     "apk": str(APK), "apk_sha256": benchlib.sha256_file(APK)},
        "provenance": prov,
        "reps": [{k: v for k, v in d.items() if k != "_path"} for d in mem_docs],
        "latency_runs": [{k: v for k, v in d.items() if k != "_path"} for d in lat_docs],
        "rows": rows,
        "verdict_counts": counts,
        "host_gate_end": benchlib.host_gate(str(REPO), check_lock=False),
    }
    stamp = utc_stamp()
    out_json = out_json or str(REPO / ("results/bench/d2-warm-%s.json" % stamp))
    with open(out_json, "w") as f:
        json.dump(doc, f, indent=2)
    table = render_markdown(doc)
    markdown = markdown or out_json[:-5] + ".md"
    with open(markdown, "w") as f:
        f.write(table)
    print("d2_warm: wrote %s" % out_json)
    print("d2_warm: wrote %s" % markdown)
    print("d2_warm: verdict counts %s" % json.dumps(counts, sort_keys=True))
    for r in rows:
        print("  %-3s %-8s n=%-3s p50=%-9s spread=%-8s %s"
              % (r["id"], r["verdict"], r["acos"]["n"],
                 _fmt(r["acos"]["value"]),
                 ("%.1f%%" % r["acos"]["stats"]["spread_pct"])
                 if (r["acos"]["stats"] or {}).get("spread_pct") is not None else "n/a",
                 (r["reasons"] or [""])[-1][:110]))
    return doc


def _fmt(v):
    return "n/a" if v is None else ("%.5g" % v)


def render_markdown(doc):
    lines = ["# D2 warm rows (held `acos serve` ART session) — %s" % doc["utc"], ""]
    lines.append("| # | Metric | ACOS-ART value | n | 95% CI (p50) | spread | state / type "
                 "| Legacy ACVM | Same type+state | Verdict |")
    lines.append("|---|---|---|---|---|---|---|---|---|---|")
    for r in doc["rows"]:
        st = r["acos"]["stats"] or {}
        ci = ("%.3g..%.3g" % (st["ci95_low"], st["ci95_high"])
              if st.get("ci95_low") is not None else "n/a")
        spread = ("%.1f%%" % st["spread_pct"]
                  if st.get("spread_pct") is not None else "n/a")
        same = "yes" if (benchlib.canonical_metric_type(r["acos"]["metric_type"])
                         == benchlib.canonical_metric_type(r["legacy"]["metric_type"])
                         and benchlib.canonical_state(r["acos"]["state"])
                         == benchlib.canonical_state(r["legacy"]["state"])) else "no"
        lines.append("| %s | %s | %s | %s | %s | %s | %s / %s | %s %s | %s | **%s** |"
                     % (r["id"], r["metric"], _fmt(r["acos"]["value"]),
                        r["acos"]["n"], ci, spread, r["acos"]["state"],
                        r["acos"]["metric_type"], r["legacy"]["value"],
                        r["legacy"]["unit"], same, r["verdict"]))
    lines.append("")
    lines.append("**Verdict counts:** " + ", ".join(
        "%d %s" % (v, k) for k, v in sorted(doc["verdict_counts"].items())))
    lines.append("")
    for r in doc["rows"]:
        lines.append("### %s — %s" % (r["id"], r["metric"]))
        for reason in r["reasons"]:
            lines.append("* " + reason)
        lines.append("")
    return "\n".join(lines)


# ---------------------------------------------------------------- self-test
def self_test():
    """No-ART analysis checks: the verdict function decides, not this script."""
    ok = True

    def check(name, cond, detail=""):
        nonlocal ok
        ok = ok and bool(cond)
        print("%s %s%s" % ("PASS" if cond else "FAIL", name,
                           (" -- " + detail) if detail and not cond else ""))

    def doc(n, total, inc, bare=10.0, contaminated=False, rep=1):
        return {"ok": True, "n": n, "rep": rep, "contaminated": contaminated,
                "total_anon_shmem_mib": total,
                "incremental_mib_per_session": inc,
                "bare": {"net_anon_shmem_mib": bare},
                "per_session": [{"i": 0, "process_anon_shmem_mib": 100.0}],
                "sessions": [{"i": 0}]}

    # M1: PASS (well under 206 * 0.95), TIE, FAIL, NOISY, and contamination.
    docs = [doc(1, 100.0 + i, None, rep=i) for i in range(5)]
    rows = {r["id"]: r for r in build_rows(docs, [])}
    check("M1 PASS below the pass line", rows["M1"]["verdict"] == "PASS",
          rows["M1"]["verdict"])
    docs = [doc(1, 205.4 + 0.01 * i, None, rep=i) for i in range(5)]
    rows = {r["id"]: r for r in build_rows(docs, [])}
    check("M1 TIE inside noise is not a pass",
          rows["M1"]["verdict"] == "TIE" and rows["M1"]["pass"] is False,
          rows["M1"]["verdict"])
    docs = [doc(1, 400.0, None, rep=i) for i in range(5)]
    rows = {r["id"]: r for r in build_rows(docs, [])}
    check("M1 FAIL above the tie band", rows["M1"]["verdict"] == "FAIL",
          rows["M1"]["verdict"])
    docs = [doc(1, 100.0 if i % 2 else 400.0, None, rep=i) for i in range(5)]
    rows = {r["id"]: r for r in build_rows(docs, [])}
    check("M1 noisy spread cannot PASS", rows["M1"]["verdict"] == "NOISY",
          rows["M1"]["verdict"])
    docs = [doc(1, 100.0, None, contaminated=True, rep=i) for i in range(4)]
    rows = {r["id"]: r for r in build_rows(docs, [])}
    check("a contaminated M1 has no samples and is NOT_MEASURED",
          rows["M1"]["verdict"] == "NOT_MEASURED", rows["M1"]["verdict"])

    # M7 latency row from synthetic latency docs.
    def lat(ms, good=True, n=20):
        return {"contaminated": False,
                "m5": [{"ms": ms + 0.01 * i, "good": good} for i in range(n)],
                "m7": [{"ms": ms + 0.01 * i, "good": good, "tree_bytes": 4000,
                        "text_nodes": 5} for i in range(n)],
                "m6": [{"ms": ms + 0.01 * i, "good": good} for i in range(10)]}
    rows = {r["id"]: r for r in build_rows([], [lat(200.0)])}
    check("M7 fast tree PASSes vs 538 ms", rows["M7"]["verdict"] == "PASS",
          rows["M7"]["verdict"])
    rows = {r["id"]: r for r in build_rows([], [lat(700.0)])}
    check("M7 700 ms FAILs vs 538 ms", rows["M7"]["verdict"] == "FAIL",
          rows["M7"]["verdict"])
    rows = {r["id"]: r for r in build_rows([], [lat(200.0, good=False)])}
    check("no good sample => NOT_MEASURED", rows["M7"]["verdict"] == "NOT_MEASURED",
          rows["M7"]["verdict"])

    # The state rule: a cold-state ACOS row can never be compared with warm legacy.
    v = benchlib.verdict(benchlib.stats([200.0] * 5), legacy_stats("M7"),
                         metric_type="WALL_MS", state="cold-process",
                         legacy_state="warm")
    check("state mismatch => NOT_COMPARABLE", v["verdict"] == "NOT_COMPARABLE",
          v["verdict"])
    # compare_memory's settle rule for the memory rows.
    cm = benchlib.compare_memory(
        {"value_mib": 300.0, "metric_type": "CGROUP_ANON_SHMEM", "state": "warm",
         "settle_s": 30},
        benchlib.legacy_memory_reference(206.0))
    check("settle mismatch => NOT_COMPARABLE",
          cm["verdict"] == "NOT_COMPARABLE", cm["verdict"])
    cm = benchlib.compare_memory(
        {"value_mib": 300.0, "metric_type": "CGROUP_ANON_SHMEM", "state": "warm",
         "settle_s": 60},
        benchlib.legacy_memory_reference(206.0))
    check("same type/state/settle => COMPARABLE",
          cm["verdict"] == "COMPARABLE", cm["verdict"])
    print("d2_warm self-test: %s" % ("ok" if ok else "FAILED"))
    return 0 if ok else 1


# ---------------------------------------------------------------- CLI
def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--acos", help="release acos binary (e.g. "
                                   ".cache/w57-target/release/acos)")
    ap.add_argument("--phase", choices=("mem", "latency"))
    ap.add_argument("--n", type=int, default=1, help="held sessions (mem phase)")
    ap.add_argument("--rep", type=int, default=1)
    ap.add_argument("--outdir", help="per-rep JSON directory (under .cache)")
    ap.add_argument("--settle", type=float, default=60.0,
                    help="seconds after the last first frame (memory settle)")
    ap.add_argument("--bare-settle", type=float, default=10.0,
                    help="seconds for the daemon-only baseline (mem phase)")
    ap.add_argument("--settle-short", type=float, default=3.0,
                    help="seconds after launch for the standalone latency phase")
    ap.add_argument("--samples", type=int, default=20, help="M5/M7 samples")
    ap.add_argument("--resets", type=int, default=10, help="M6 samples")
    ap.add_argument("--warmup", type=int, default=2, help="discarded warm-ups per verb")
    ap.add_argument("--with-latency", action="store_true",
                    help="measure M5/M6/M7 on the settled session of this mem rep")
    ap.add_argument("--keep-scratch", action="store_true")
    ap.add_argument("--analyze", help="aggregate an outdir into the D2-warm result")
    ap.add_argument("--out")
    ap.add_argument("--markdown")
    ap.add_argument("--self-test", action="store_true")
    args = ap.parse_args(argv)

    if args.self_test:
        return self_test()
    if args.analyze:
        analyze(args.analyze, args.out, args.markdown)
        return 0
    if not args.acos or not args.phase or not args.outdir:
        ap.error("--acos, --phase and --outdir are required")
    if "/release/" not in args.acos.replace(os.sep, "/"):
        print("d2_warm: refusing a non-release binary path %s" % args.acos,
              file=sys.stderr)
        return 2
    Path(args.outdir).mkdir(parents=True, exist_ok=True)
    if args.phase == "mem":
        doc = run_mem_phase(args, args.outdir)
    else:
        doc = run_latency_phase(args, args.outdir)
    return 0 if doc.get("ok") else 1


if __name__ == "__main__":
    sys.exit(main())
