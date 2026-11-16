#!/usr/bin/env python3
"""p7.py -- P7: ACOS M3 (cold) and M4 (warm) launch latency, plus the launch
phase ladder.

Diagnosis only. Nothing in ``crates/`` is changed; every measurement goes
through the release binaries' own public surface:

  * M4 / M3 use the daemon an agent uses (`acos serve` + newline-JSON on its
    unix socket) and the two verbs an agent uses (`acos_launch`,
    `acos_observe`, `acos_observe_tree`, `acos_destroy`).
  * `phases` spawns `acos-art` with exactly the argv/environment
    `crates/acos-cli/src/engine/art.rs::ArtSession::start` uses, so the host
    phase ladder (`ACOS_ART_STAGE_TIMING`, an existing env-gated trace) and the
    guest marker ladder (`ACOSLAUNCH_GUEST_STDOUT_FILE`, an existing tee) line
    up with the real daemon path.

Definitions follow results/art-runtime/P6.md:

  M3  nothing running                -> start ACOS + app -> first correct frame
  M4  daemon up, app not running     -> launch           -> first correct frame

A frame is "correct" only when it is byte-identical to a reference frame that
itself was accepted only when (a) the live tree reported the counter text `0`,
(b) two consecutive captures of the settled screen were identical. A row is
verified only when the tree read *after* that frame still reports `0`
(P6's "each row re-reads the tree after the frame").

Usage (inside run-exclusive.sh + guarded.sh):

    p7.py all --out DIR
    p7.py analyze --dir DIR
"""

import argparse
import json
import os
import shutil
import socket
import subprocess
import sys
import threading
import time
from datetime import datetime, timezone
from pathlib import Path

HERE = Path(__file__).resolve().parent
REPO = HERE.parent.parent.parent.parent          # results/art-runtime/evidence/P7
sys.path.insert(0, str(REPO / "tools"))
sys.path.insert(0, str(REPO / "tools/bench"))
import benchlib  # noqa: E402
import bench_reply  # noqa: E402  (BENCHFIX: the reply/tree contract)

BIN = REPO / "target/release/acos"
ART_BIN = REPO / "target/release/acos-art"
APK = REPO / "fixtures/build/apps/counter.apk"
SYSROOT = REPO / ".cache/art/sysroot"
COUNT_RID = "dev.acos.fixtures.counter:id/count"
EXPECT_TEXT = "0"
EXPECT_NODES = 12


def utc():
    return datetime.now(timezone.utc).isoformat(timespec="milliseconds")


def host_now():
    h = {**benchlib.read_meminfo(), **benchlib.read_loadavg()}
    return h


def fadv_drop(paths):
    """Evict clean page-cache pages of `paths` without root (POSIX_FADV_DONTNEED).

    Returns {path: {"before": cached_bytes, "after": cached_bytes}}. `fincore`
    is the independent check that the pages really went away.
    """
    out = {}
    for p in paths:
        p = Path(p)
        if not p.exists():
            continue
        try:
            before = fincore_cached(p)
            fd = os.open(str(p), os.O_RDONLY)
            try:
                os.posix_fadvise(fd, 0, 0, os.POSIX_FADV_DONTNEED)
            finally:
                os.close(fd)
            after = fincore_cached(p)
            out[str(p)] = {"before": before, "after": after, "size": p.stat().st_size}
        except OSError as exc:                      # never fatal
            out[str(p)] = {"error": str(exc)}
    return out


def fincore_cached(path):
    """Resident (page-cache) bytes of `path`, straight from util-linux fincore."""
    r = subprocess.run(["fincore", "--bytes", str(path)], capture_output=True, text=True)
    line = r.stdout.strip().splitlines()
    if len(line) > 1:
        cols = line[1].split()
        if cols and cols[0].isdigit():
            return int(cols[0])
    return None


def cold_files():
    """The files a launch must read before ART draws anything."""
    pats = [
        SYSROOT / "system/framework/x86_64/boot.art",
        SYSROOT / "system/framework/x86_64/boot.oat",
        SYSROOT / "system/framework/boot.vdex",
        SYSROOT / "system/framework/x86_64/boot.vdex",
        SYSROOT / "system/framework/framework.jar",
        SYSROOT / "system/framework/am.jar",
        SYSROOT / "system/lib64/libart.so",
        SYSROOT / "system/lib64/libartbase.so",
        SYSROOT / "system/lib64/libnativehelper.so",
        SYSROOT / "system/lib64/libandroid_runtime.so",
        APK,
    ]
    for p in SYSROOT.rglob("dalvikvm64"):
        pats.append(p)
    for p in sorted(SYSROOT.glob("system/framework/*.jar")):
        pats.append(p)
    return pats


class Daemon:
    """The real transport: newline JSON on the daemon's unix socket."""

    def __init__(self, path, timeout=180.0):
        self.sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        self.sock.settimeout(timeout)
        self.sock.connect(str(path))
        self.file = self.sock.makefile("rwb")
        self.n = 0

    def call(self, tool, args):
        self.n += 1
        t0 = time.perf_counter()
        self.file.write((json.dumps({"id": self.n, "tool": tool, "args": args},
                                    separators=(",", ":")) + "\n").encode())
        self.file.flush()
        line = self.file.readline()
        ms = (time.perf_counter() - t0) * 1000.0
        if not line:
            raise ConnectionError("daemon closed the connection during %s" % tool)
        return ms, json.loads(line)

    def close(self):
        try:
            self.file.close()
        finally:
            self.sock.close()


def nodes_of(tree):
    def walk(ns):
        for n in ns or []:
            if isinstance(n, dict):
                yield n
                yield from walk(n.get("children"))
    return list(walk((tree or {}).get("nodes")))


def count_text(tree):
    for n in nodes_of(tree):
        rid = n.get("resource_id") or ""
        if rid == COUNT_RID or rid.endswith(":id/count"):
            return n.get("text")
    return None


def tree_ok(tree):
    """The counter's settled, correct screen (count 0, fixture ids present).

    BENCHFIX: this used to be `len(nodes_of(tree)) == EXPECT_NODES` (12), a
    measurement of one ART revision written down in P6.  The same verified screen
    walks 14 nodes since v0.2.2, so every M3 run refused its own reference
    (`reference tree is not the count-0 screen: count='0' nodes=14`).  A measured
    node count is not a correctness rule; `bench_reply.counter_screen_problems`
    checks what the rule actually means (the count node says 0, the fixture's
    `count`/`inc`/`reset` nodes are laid out, the walk is not truncated, and the
    node set is not a skeleton) and needs no magic number.
    """
    return not bench_reply.counter_screen_problems(tree)


def screen(tree, where):
    """The full tree of a reply (inline on v0.2.2-, `tree_path` file on rel-030+)."""
    return bench_reply.full_tree(tree, where)


def start_daemon(scratch, tag):
    """Spawn `acos serve` and return (proc, socket_path, ms_to_socket)."""
    sock = scratch / ("s-%s.sock" % tag)
    if sock.exists():
        sock.unlink()
    env = dict(os.environ)
    env["ACOS_RUNTIME_DIR"] = str(scratch / ("rt-%s" % tag))
    env["ACOS_CACHE_DIR"] = str(scratch / ("cache-%s" % tag))
    t0 = time.perf_counter()
    proc = subprocess.Popen(
        [str(BIN), "serve", "--socket", str(sock), "--idle-timeout", "600",
         "--max-sessions", "8"],
        env=env, stdout=subprocess.DEVNULL,
        stderr=open(scratch / ("serve-%s.err" % tag), "wb"))
    while time.perf_counter() - t0 < 30.0:
        if sock.exists():
            return proc, sock, (time.perf_counter() - t0) * 1000.0
        if proc.poll() is not None:
            raise RuntimeError("serve exited during startup")
        time.sleep(0.0005)
    raise RuntimeError("serve socket never appeared")


def stop_daemon(proc):
    if proc is None:
        return
    proc.terminate()
    try:
        proc.wait(10)
    except subprocess.TimeoutExpired:
        proc.kill()
        proc.wait(10)


def make_reference(scratch):
    """One warm launch -> the verified count-0 reference frame (P6 rule)."""
    proc, sock, sock_ms = start_daemon(scratch, "ref")
    try:
        d = Daemon(sock)
        base = {"session_id": "s_ref"}
        # BENCHFIX: `pixels: true` is what makes the launch reply's frame a real
        # capture with an md5 (on rel-030 a metadata-only frame carries ""), and the
        # screen is read from the full tree the reply points at, not from a compact
        # page.  The two captures are frame-only `acos_observe` replies: FB2c
        # confines a `screenshot: <path>` to the session's artifacts directory, so
        # the old `<scratch>/ref-N.png` was an argument error and both captures
        # came back with no frame at all.
        ms, r = d.call("acos_launch", {"base": base, "apk": str(APK), "pixels": True})
        res, res_tree = bench_reply.require_launch(r.get("envelope"), "reference launch")
        bench_reply.require_counter_screen(res_tree, "reference launch")
        md5s = []
        for i in (1, 2):
            _ms, rr = d.call("acos_observe", {"base": base, "tree": False})
            _value, md5 = bench_reply.require_capture(
                rr.get("envelope"), "reference capture %d" % i)
            md5s.append(md5)
        md5 = bench_reply.identical_captures(md5s, "reference")
        # the launch reply's bound frame must be the same settled screen
        launch_md5 = bench_reply.frame_md5(res, "reference launch")
        if launch_md5 != md5:
            raise RuntimeError("launch frame %s != settled capture %s"
                               % (launch_md5, md5))
        d.call("acos_destroy", {"base": base})
        d.close()
        return {"md5": md5, "nodes": len(nodes_of(res_tree)),
                "count_text": count_text(res_tree), "daemon_start_ms": sock_ms,
                "launch_ms": ms}
    finally:
        stop_daemon(proc)


def one_launch(d, base, ref_md5, scratch, tag, t0, t0_utc):
    """Launch -> first correct frame -> strict re-read.

    All times are relative to the caller's `t0` (perf_counter) / `t0_utc`:
    for M4 that is the launch request, for M3 the cold start (daemon spawn).
    """
    row = {"ok": False}
    t_launch_send = time.perf_counter()
    launch_utc = utc()
    row["launch_start_ms"] = (t_launch_send - t0) * 1000.0
    # BENCHFIX: pixels give the reply a real capture md5 (M3 compares it to the
    # reference); the screen is the full tree, never the compact page.
    launch_ms, reply = d.call("acos_launch", {"base": base, "apk": str(APK),
                                              "pixels": True})
    env = reply.get("envelope") or {}
    row["launch_wall_ms"] = launch_ms
    if not env.get("ok"):
        row["error"] = (env.get("error") or {}).get("code") or "launch-failed"
        return row
    res = env["result"]
    launch_tree = screen(res, "M3/M4 launch")
    row["launch_tree_ok"] = tree_ok(launch_tree)
    row["launch_count_text"] = count_text(launch_tree)
    row["launch_nodes"] = len(nodes_of(launch_tree))
    row["launch_frame_md5"] = (res.get("frame") or {}).get("md5")
    row["launch_frame_seq"] = (res.get("frame") or {}).get("frame_seq")
    row["launch_frame_captured_at"] = (res.get("frame") or {}).get("captured_at")
    at = row["launch_frame_captured_at"]
    if at:
        row["launch_frame_landed_ms"] = t_ms_since(at, launch_utc)
    polls = 0
    accepted = None
    if row["launch_frame_md5"] == ref_md5:
        accepted = ("launch", launch_ms, row.get("launch_frame_landed_ms"))
    else:
        # The launch reply's bound frame is not the settled reference: keep
        # observing real captures until one is (P6's screencap poll).
        while polls < 30:
            polls += 1
            ms, r = d.call("acos_observe", {"base": base, "tree": False})
            fr = ((r.get("envelope") or {}).get("result") or {}).get("frame") or {}
            if fr.get("md5") == ref_md5:
                accepted = ("poll", (time.perf_counter() - t_launch_send) * 1000.0,
                            t_ms_since(fr.get("captured_at"), launch_utc))
                break
    row["polls"] = polls
    if accepted is None:
        row["error"] = "no-correct-frame"
        return row
    row["accepted_on"] = accepted[0]
    # m_ms: from the rep's t0 to the accepted frame, client-observed (poll
    # granularity included); frame_landed_ms: the same moment taken from the
    # frame's own capture timestamp (poll-free, physical).
    row["m_ms"] = row["launch_start_ms"] + accepted[1]
    row["frame_landed_ms"] = (None if accepted[2] is None
                              else row["launch_start_ms"] + accepted[2])
    # P6's rule: re-read the tree AFTER the accepted frame.
    tv_ms, tv = d.call("acos_observe_tree", {"base": base})
    vres, vtree = bench_reply.require_tree_read(tv.get("envelope"),
                                                "M3/M4 verify tree")
    row["verify_tree_wall_ms"] = tv_ms
    row["verify_count_text"] = count_text(vtree)
    row["verify_nodes"] = len(nodes_of(vtree))
    row["verified"] = tree_ok(vtree)
    row["total_wall_ms"] = (time.perf_counter() - t0) * 1000.0
    row["ok"] = bool(row["verified"])
    return row


def t_ms_since(iso, t0_iso):
    """Capture wall clock (rfc3339, ms) minus the rep's t0 (rfc3339)."""
    def parse(s):
        return datetime.strptime(s.replace("Z", "+0000"), "%Y-%m-%dT%H:%M:%S.%f%z").timestamp()
    try:
        return (parse(iso) - parse(t0_iso)) * 1000.0
    except Exception:
        return None


def rep_tagged(scratch, i):
    return "%d" % i


def phase_m4(scratch, reps, ref, out):
    proc, sock, sock_ms = start_daemon(scratch, "m4")
    rows = []
    try:
        d = Daemon(sock)
        # warm-up: one launch+destroy, discarded (P6's "previous process warm")
        b = {"session_id": "s_warm0"}
        d.call("acos_launch", {"base": b, "apk": str(APK)})
        d.call("acos_destroy", {"base": b})
        for i in range(reps):
            base = {"session_id": "s_m4_%d" % i}
            row = {"rep": i, "phase": "M4", "host": host_now()}
            t0, t0_utc = time.perf_counter(), utc()
            r = one_launch(d, base, ref["md5"], scratch, "m4-%d" % i, t0, t0_utc)
            row.update(r)
            dms, dr = d.call("acos_destroy", {"base": base})
            row["destroy_wall_ms"] = dms
            row["destroy_clean"] = bool(((dr.get("envelope") or {}).get("result") or {})
                                        .get("teardown", {}).get("verified"))
            rows.append(row)
            print("M4 rep %2d launch=%.1f total=%.1f accepted=%s verified=%s"
                  % (i, row.get("launch_wall_ms", -1), row.get("m_ms", -1),
                     row.get("accepted_on"), row.get("ok")), flush=True)
        d.close()
    finally:
        stop_daemon(proc)
    payload = {"phase": "M4", "rows": rows, "daemon_start_ms": sock_ms,
               "definition": "daemon already up, app not running -> acos_launch -> "
                             "first frame byte-identical to the verified count-0 reference",
               "utc": utc()}
    benchlib.write_json(str(out), payload)
    return payload


def phase_m3(scratch, reps, ref, out, drop):
    rows = []
    for i in range(reps):
        tag = "m3c%d" % i if drop else "m3%d" % i
        row = {"rep": i, "phase": "M3-cold-cache" if drop else "M3", "utc": utc(),
               "host": host_now()}
        if drop:
            row["fadvise"] = fadv_drop(cold_files())
        t0, t0_utc = time.perf_counter(), utc()
        proc, sock, sock_ms = start_daemon(scratch, tag)
        row["daemon_start_ms"] = sock_ms
        try:
            d = Daemon(sock)
            base = {"session_id": "s_m3_%d" % i}
            r = one_launch(d, base, ref["md5"], scratch, tag, t0, t0_utc)
            row.update(r)
            row["m3_ms"] = (time.perf_counter() - t0) * 1000.0
            if r.get("frame_landed_ms") is not None and r.get("launch_start_ms") is not None:
                row["cold_frame_ms"] = r["launch_start_ms"] + r["frame_landed_ms"]
            if r.get("ok"):
                dms, dr = d.call("acos_destroy", {"base": base})
                row["destroy_wall_ms"] = dms
            d.close()
        finally:
            stop_daemon(proc)
        rows.append(row)
        print("%s rep %2d daemon=%.1f launch=%.1f total=%.1f verified=%s"
              % ("M3cold" if drop else "M3   ", i, row["daemon_start_ms"],
                 row.get("launch_wall_ms", -1), row.get("m3_ms", -1), row.get("ok")),
              flush=True)
    payload = {"phase": "M3-cold-cache" if drop else "M3", "rows": rows,
               "definition": "nothing running -> acos serve + acos_launch -> first frame "
                             "byte-identical to the verified count-0 reference",
               "page_cache_dropped": bool(drop), "utc": utc()}
    benchlib.write_json(str(out), payload)
    return payload


# --------------------------------------------------------------- phase ladder
STAGE_RE = None


def phase_phases(scratch, reps, out):
    """Spawn acos-art exactly as ArtSession::start does, and time the ladder."""
    rows = []
    for i in range(reps):
        run = scratch / ("ph%d" % i)
        frames = run / "art-frames"
        frames.mkdir(parents=True, exist_ok=True)
        tee = run / "guest-stdout.log"
        tee.write_text("")
        env = dict(os.environ)
        env.update({
            "ACOSLAUNCH_INTERACTIVE": "1",
            "ACOSLAUNCH_LIVE_PROBE": "1",
            "ACOSLAUNCH_FRAME_WAIT_SECS": "3",     # art.rs DEFAULT_FRAME_WAIT_SECS
            "ACOS_ART_STAGE_TIMING": "1",
            "ACOSLAUNCH_GUEST_STDOUT_FILE": str(tee),
        })
        t0 = time.perf_counter()
        proc = subprocess.Popen(
            [str(ART_BIN), "launch", "--apk", str(APK), "--until", "activity-resumed",
             "--out", str(frames)],
            env=env, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
            stderr=subprocess.PIPE, text=True, bufsize=1, process_group=0)
        host_stages, guest_lines, ready = [], [], None
        lock = threading.Lock()

        def read_err(p=proc):
            for line in p.stderr:
                ms = (time.perf_counter() - t0) * 1000.0
                if "acos-art: stage " in line:
                    name = line.split("acos-art: stage ", 1)[1].split()[0]
                    with lock:
                        host_stages.append({"name": name, "recv_ms": ms, "line": line.strip()})

        def read_out(p=proc):
            nonlocal ready
            for line in p.stdout:
                ms = (time.perf_counter() - t0) * 1000.0
                line = line.strip()
                if not line:
                    continue
                if '"cmd":"ready"' in line or '"cmd": "ready"' in line:
                    with lock:
                        ready = {"recv_ms": ms, "line": line}
                    break

        def tail(p=tee):
            # guest markers, timestamped on host receipt (tee write is a flush
            # per line: System.out is autoflush on println)
            pos = 0
            while True:
                try:
                    with open(p, "r") as fh:
                        fh.seek(pos)
                        chunk = fh.read()
                        pos = fh.tell()
                except FileNotFoundError:
                    chunk = ""
                if chunk:
                    ms = (time.perf_counter() - t0) * 1000.0
                    for line in chunk.splitlines():
                        if line.strip():
                            with lock:
                                guest_lines.append({"recv_ms": ms, "line": line.strip()})
                if done.is_set():
                    return
                time.sleep(0.001)

        done = threading.Event()
        ts = [threading.Thread(target=read_err, daemon=True),
              threading.Thread(target=read_out, daemon=True),
              threading.Thread(target=tail, daemon=True)]
        for t in ts:
            t.start()
        deadline = t0 + 60
        while ready is None and time.perf_counter() < deadline and proc.poll() is None:
            time.sleep(0.002)
        ready_ms = ready["recv_ms"] if ready else None
        # On the same live session: what one strict `tree` command costs, i.e.
        # the settle+capture the daemon's launch reply adds on top of `ready`.
        tree_probe = []
        if ready_ms is not None and proc.poll() is None:
            time.sleep(0.02)          # let the ready-line reader release stdout
            for k in (1, 2):
                t_send = time.perf_counter()
                try:
                    proc.stdin.write('{"cmd":"tree"}\n')
                    proc.stdin.flush()
                except Exception:
                    break
                line = None
                deadline2 = t_send + 20
                while time.perf_counter() < deadline2:
                    line = proc.stdout.readline()
                    if line and '"cmd":"tree"' in line:
                        break
                    line = None
                tree_probe.append({"k": k, "ms": None if line is None
                                   else (time.perf_counter() - t_send) * 1000.0,
                                   "count_zero": bool(line and '"text":"0"' in line),
                                   "bytes": len(line or "")})
        done.set()
        time.sleep(0.05)
        if proc.poll() is None:
            try:
                proc.stdin.write('{"cmd":"close"}\n')
                proc.stdin.flush()
            except Exception:
                pass
        try:
            proc.wait(30)
        except subprocess.TimeoutExpired:
            proc.kill()
            proc.wait(10)
        for t in ts:
            t.join(1.0)
        rows.append({"rep": i, "utc": utc(), "host": host_now(),
                     "ready_ms": ready_ms, "ready_line": (ready or {}).get("line"),
                     "host_stages": sorted(host_stages, key=lambda s: s["recv_ms"]),
                     "guest_lines": guest_lines, "tree_probe": tree_probe,
                     "rc": proc.returncode})
        print("phases rep %d ready=%.1f ms stages=%d guestlines=%d rc=%s"
              % (i, ready_ms or -1, len(host_stages), len(guest_lines), proc.returncode),
              flush=True)
        shutil.rmtree(run, ignore_errors=True)
    payload = {"phase": "phases", "rows": rows, "utc": utc(),
               "argv": "acos-art launch --apk <counter.apk> --until activity-resumed "
                       "--out <art-frames>  (env: ACOSLAUNCH_INTERACTIVE=1, "
                       "ACOSLAUNCH_LIVE_PROBE=1, ACOSLAUNCH_FRAME_WAIT_SECS=3)"}
    benchlib.write_json(str(out), payload)
    return payload


def main(argv):
    ap = argparse.ArgumentParser()
    ap.add_argument("mode", choices=["all", "ref", "m4", "m3", "phases", "analyze"])
    ap.add_argument("--out", default=str(HERE / "run"))
    ap.add_argument("--dir", default=None)
    ap.add_argument("--m4", type=int, default=24)
    ap.add_argument("--m3", type=int, default=12)
    ap.add_argument("--m3-cold", type=int, default=12)
    ap.add_argument("--phases", type=int, default=8)
    a = ap.parse_args(argv)
    out = Path(a.out).resolve()
    out.mkdir(parents=True, exist_ok=True)
    scratch = out / "scratch"
    scratch.mkdir(exist_ok=True)
    for p in (BIN, ART_BIN, APK):
        if not p.exists():
            print("p7: missing %s" % p, file=sys.stderr)
            return 2
    if "release" not in str(BIN):
        print("p7: refusing a non-release binary", file=sys.stderr)
        return 2
    if a.mode == "analyze":
        return analyze(Path(a.dir or out))
    meta = {"utc": utc(), "bin": str(BIN), "acos_sha256": benchlib.sha256_file(BIN),
            "acos_art_sha256": benchlib.sha256_file(ART_BIN),
            "apk_sha256": benchlib.sha256_file(APK),
            "host": host_now(), "loadavg_at_start": benchlib.read_loadavg()}
    ref = json.loads((out / "ref.json").read_text()) if (out / "ref.json").exists() else None
    if a.mode in ("all", "ref") and ref is None:
        print("p7: reference frame", flush=True)
        ref = make_reference(scratch)
        benchlib.write_json(str(out / "ref.json"), ref)
        print("p7: reference md5 %s (%d nodes, count %r)"
              % (ref["md5"], ref["nodes"], ref["count_text"]), flush=True)
    if a.mode == "ref":
        return 0
    if a.mode in ("all", "phases"):
        phase_phases(scratch, a.phases, out / "phases.json")
    if a.mode in ("all", "m3"):
        phase_m3(scratch, a.m3, ref, out / "m3.json", drop=False)
    if a.mode in ("all", "m3"):
        phase_m3(scratch, a.m3_cold, ref, out / "m3-cold.json", drop=True)
    if a.mode in ("all", "m4"):
        phase_m4(scratch, a.m4, ref, out / "m4.json")
    benchlib.write_json(str(out / "meta.json"), meta)
    return analyze(out)


# ------------------------------------------------------------------- analysis
def pct(vals, q):
    return benchlib.percentile(sorted(vals), q)


def summarize(rows, key):
    vals = [r[key] for r in rows if isinstance(r.get(key), (int, float))]
    if not vals:
        return None
    return {"n": len(vals), "p50": pct(vals, 50), "p95": pct(vals, 95),
            "min": min(vals), "max": max(vals),
            "spread_pct": (max(vals) - min(vals)) / pct(vals, 50) * 100.0 if pct(vals, 50) else None}


def analyze(out):
    out = Path(out)
    rp = out / "ref.json"
    ref = json.loads(rp.read_text()) if rp.exists() else None
    res = {"utc": utc(), "reference": ref, "tables": {}}
    rows = {}
    for name in ("m3", "m3-cold", "m4"):
        p = out / ("%s.json" % name)
        if p.exists():
            rows[name] = json.loads(p.read_text())["rows"]
    # M3 (all measured reps)
    for name in ("m3", "m3-cold"):
        if name in rows:
            rs = rows[name]
            res["tables"][name] = {
                "n_reps": len(rs), "n_verified": sum(1 for r in rs if r.get("ok")),
                "daemon_start_ms": summarize(rs, "daemon_start_ms"),
                "launch_wall_ms": summarize(rs, "launch_wall_ms"),
                "frame_landed_ms": summarize(rs, "frame_landed_ms"),
                "m3_total_ms": summarize(rs, "m3_ms"),
                "verify_tree_wall_ms": summarize(rs, "verify_tree_wall_ms"),
                "destroy_wall_ms": summarize(rs, "destroy_wall_ms"),
            }
    if "m4" in rows:
        rs = rows["m4"]
        res["tables"]["m4"] = {
            "n_reps": len(rs), "n_verified": sum(1 for r in rs if r.get("ok")),
            "launch_wall_ms": summarize(rs, "launch_wall_ms"),
            "frame_landed_ms": summarize(rs, "frame_landed_ms"),
            "destroy_wall_ms": summarize(rs, "destroy_wall_ms"),
            "verify_tree_wall_ms": summarize(rs, "verify_tree_wall_ms"),
            "accepted_on": {k: sum(1 for r in rs if r.get("accepted_on") == k)
                            for k in ("launch", "poll")},
            "polls": summarize(rs, "polls"),
        }
    # phase ladder: median receive time of each named stage/marker
    p = out / "phases.json"
    if p.exists():
        prs = [r for r in json.loads(p.read_text())["rows"] if r.get("ready_ms")]
        host = {}
        guest = {}
        for r in prs:
            for s in r["host_stages"]:
                host.setdefault(s["name"], []).append(s["recv_ms"])
            for g in r["guest_lines"]:
                name = g["line"].replace("ACOSLAUNCH MARK ", "").split()[0]
                if name in ("ACOSLAUNCH", "ACOSPROBE", "ACOSPROBE-ROOTS"):
                    continue          # probe/outcome lines, not ladder markers
                guest.setdefault(name, []).append(g["recv_ms"])
            host.setdefault("__ready__", []).append(r["ready_ms"])
            for k in r.get("tree_probe") or []:
                if k.get("ms") is not None:
                    host.setdefault("tree_cmd_%d" % k["k"], []).append(k["ms"])
        res["phase_ladder"] = {
            "n": len(prs),
            "host": {k: {"p50": pct(v, 50), "p95": pct(v, 95), "n": len(v)}
                     for k, v in host.items()},
            "guest": {k: {"p50": pct(v, 50), "p95": pct(v, 95), "n": len(v)}
                      for k, v in guest.items()},
        }
        # ordered ladder, p50, plus delta from the previous row
        order = sorted(res["phase_ladder"]["host"].items(), key=lambda kv: kv[1]["p50"])
        ladder, prev = [], 0.0
        for name, st in order:
            if st["n"] < len(prs) or name.startswith("tree_cmd_") \
                    or name.startswith("teardown") or name in ("guest-exit",):
                continue
            ladder.append({"kind": "host", "name": name, "p50": st["p50"],
                           "delta": st["p50"] - prev, "n": st["n"]})
            prev = st["p50"]
        res["phase_ladder"]["ordered_host"] = ladder
        order = sorted(res["phase_ladder"]["guest"].items(), key=lambda kv: kv[1]["p50"])
        ladder, prev = [], 0.0
        for name, st in order:
            if st["n"] < len(prs):
                continue
            ladder.append({"kind": "guest", "name": name, "p50": st["p50"],
                           "delta": st["p50"] - prev, "n": st["n"]})
            prev = st["p50"]
        res["phase_ladder"]["ordered_guest"] = ladder
        res["phase_ladder"]["tree_probe"] = {
            k: v for k, v in res["phase_ladder"]["host"].items()
            if k.startswith("tree_cmd_")}
    benchlib.write_json(str(out / "summary.json"), res)
    print(json.dumps({k: v for k, v in res["tables"].items()}, indent=2)[:4000])
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
