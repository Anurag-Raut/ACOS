#!/usr/bin/env python3
"""p8.py -- P8: does a startup page-cache prewarm remove P7's +198 ms cold launch?

Reuses P7's state definitions, eviction method and reference-frame rule
(``results/art-runtime/evidence/P7/p7.py``):

  * the launch working set is evicted with POSIX_FADV_DONTNEED over the same
    44 files (no sudo, no /proc/sys/vm/drop_caches) and ``fincore`` proves the
    pages went away;
  * a frame counts only when it is byte-identical to a reference that was
    accepted on a live count-0 tree plus two identical settled captures, and a
    row counts only when the tree re-read *after* the frame still reports 0;
  * M3 = nothing running -> ``acos serve`` + ``acos_launch`` -> frame;
    M4 = daemon up, app not running -> ``acos_launch`` -> frame.

Arms:

  m3-off     evict, then start the daemon with ACOS_NO_PREWARM=1 (the old
             behaviour: no prewarm) and launch immediately
  m3-on      evict, start the daemon with the prewarm on and launch immediately
             (the product path: the socket is ready in ~2 ms and the launch
             races the background prewarm)
  m3-on-wait evict, start the daemon with the prewarm on and send the launch
             only after the daemon logged that the warm finished (best case)
  m4         daemon up (prewarm on), app not running -> 12 launches

Also records the daemon-ready time (`sock_ms`) for every rep and the daemon's
RSS before/after the prewarm (page cache is not RSS; the prewarm must not move
it).
"""

import argparse
import json
import os
import socket
import subprocess
import sys
import time
from datetime import datetime, timezone
from pathlib import Path

HERE = Path(__file__).resolve().parent
REPO = HERE.parent.parent.parent.parent
sys.path.insert(0, str(REPO / "tools"))
import benchlib  # noqa: E402

BIN = REPO / "target/release/acos"
APK = REPO / "fixtures/build/apps/counter.apk"
SYSROOT = REPO / ".cache/art/sysroot"
COUNT_RID = "dev.acos.fixtures.counter:id/count"
EXPECT_TEXT = "0"
EXPECT_NODES = 12


def utc():
    return datetime.now(timezone.utc).isoformat(timespec="milliseconds")


def host_now():
    return {**benchlib.read_meminfo(), **benchlib.read_loadavg()}


# --------------------------------------------------------------- eviction (P7)
def fincore_cached(path):
    r = subprocess.run(["fincore", "--bytes", str(path)], capture_output=True, text=True)
    lines = r.stdout.strip().splitlines()
    if len(lines) > 1:
        cols = lines[1].split()
        if cols and cols[0].isdigit():
            return int(cols[0])
    return None


def cold_files():
    """P7's exact 44-file launch working set."""
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


def fadv_drop(paths):
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
            out[str(p)] = {"before": before, "after": fincore_cached(p),
                           "size": p.stat().st_size}
        except OSError as exc:
            out[str(p)] = {"error": str(exc)}
    return out


# ------------------------------------------------------------------- daemon IO
class Daemon:
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


def rss_kb(pid):
    try:
        for line in Path("/proc/%d/status" % pid).read_text().splitlines():
            if line.startswith("VmRSS:"):
                return int(line.split()[1])
    except OSError:
        return None
    return None


def start_daemon(scratch, tag, no_prewarm):
    sock = scratch / ("s-%s.sock" % tag)
    if sock.exists():
        sock.unlink()
    err = scratch / ("serve-%s.err" % tag)
    env = dict(os.environ)
    env["ACOS_RUNTIME_DIR"] = str(scratch / ("rt-%s" % tag))
    env["ACOS_CACHE_DIR"] = str(scratch / ("cache-%s" % tag))
    env.pop("ACOS_NO_PREWARM", None)
    if no_prewarm:
        env["ACOS_NO_PREWARM"] = "1"
    t0 = time.perf_counter()
    proc = subprocess.Popen(
        [str(BIN), "serve", "--socket", str(sock), "--idle-timeout", "600",
         "--max-sessions", "8"],
        env=env, stdout=subprocess.DEVNULL, stderr=open(err, "wb"))
    while time.perf_counter() - t0 < 30.0:
        if sock.exists():
            return proc, sock, (time.perf_counter() - t0) * 1000.0, err
        if proc.poll() is not None:
            raise RuntimeError("serve exited during startup")
        time.sleep(0.0005)
    raise RuntimeError("serve socket never appeared")


def prewarm_log(err):
    """The daemon's prewarm summary, if the thread already wrote it."""
    try:
        text = err.read_text()
    except OSError:
        return None
    for line in text.splitlines():
        if "prewarm:" in line:
            return line.split("prewarm:", 1)[1].strip()
    return None


def wait_prewarm(err, timeout=10.0):
    t0 = time.perf_counter()
    while time.perf_counter() - t0 < timeout:
        line = prewarm_log(err)
        if line:
            return (time.perf_counter() - t0) * 1000.0, line
        time.sleep(0.001)
    return None, None


def stop_daemon(proc):
    if proc is None:
        return
    proc.terminate()
    try:
        proc.wait(10)
    except subprocess.TimeoutExpired:
        proc.kill()
        proc.wait(10)


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
    return (count_text(tree) == EXPECT_TEXT and len(nodes_of(tree)) == EXPECT_NODES)


def make_reference(scratch):
    proc, sock, sock_ms, err = start_daemon(scratch, "ref", no_prewarm=False)
    try:
        d = Daemon(sock)
        base = {"session_id": "s_ref"}
        ms, r = d.call("acos_launch", {"base": base, "apk": str(APK)})
        env = r.get("envelope") or {}
        if not env.get("ok"):
            raise RuntimeError("reference launch failed: %s" % json.dumps(r)[:400])
        res = env["result"]
        if not tree_ok(res.get("tree")):
            raise RuntimeError("reference tree is not the count-0 screen: count=%r nodes=%d"
                               % (count_text(res.get("tree")), len(nodes_of(res.get("tree")))))
        shots = []
        for i in (1, 2):
            p = scratch / ("ref-%d.png" % i)
            ms, r = d.call("acos_observe", {"base": base, "png": True, "screenshot": str(p)})
            shots.append(r["envelope"]["result"]["frame"])
        md5s = [s["md5"] for s in shots]
        if md5s[0] != md5s[1]:
            raise RuntimeError("two consecutive settled captures differ: %s" % md5s)
        if res.get("frame", {}).get("md5") != md5s[0]:
            raise RuntimeError("launch frame %s != settled capture %s"
                               % (res.get("frame", {}).get("md5"), md5s[0]))
        d.call("acos_destroy", {"base": base})
        d.close()
        return {"md5": md5s[0], "nodes": len(nodes_of(res["tree"])),
                "count_text": count_text(res["tree"]), "launch_ms": ms}
    finally:
        stop_daemon(proc)


def one_launch(d, base, ref_md5, launch_start_ms, launch_utc):
    row = {"ok": False}
    launch_ms, reply = d.call("acos_launch", {"base": base, "apk": str(APK)})
    env = reply.get("envelope") or {}
    row["launch_wall_ms"] = launch_ms
    if not env.get("ok"):
        row["error"] = (env.get("error") or {}).get("code") or "launch-failed"
        return row
    res = env["result"]
    row["launch_tree_ok"] = tree_ok(res.get("tree"))
    row["launch_count_text"] = count_text(res.get("tree"))
    row["launch_frame_md5"] = (res.get("frame") or {}).get("md5")
    row["launch_frame_landed_ms"] = None
    at = (res.get("frame") or {}).get("captured_at")
    if at:
        try:
            parse = lambda s: datetime.strptime(s.replace("Z", "+0000"),
                                                "%Y-%m-%dT%H:%M:%S.%f%z").timestamp()
            row["launch_frame_landed_ms"] = (parse(at) - parse(launch_utc)) * 1000.0
        except Exception:
            pass
    row["accepted_on"] = "launch"
    row["m_ms"] = launch_start_ms + launch_ms
    if row["launch_frame_landed_ms"] is not None:
        row["frame_landed_ms"] = launch_start_ms + row["launch_frame_landed_ms"]
    tv_ms, tv = d.call("acos_observe_tree", {"base": base})
    vres = (tv.get("envelope") or {}).get("result") or {}
    row["verify_count_text"] = count_text(vres.get("tree"))
    row["verify_nodes"] = len(nodes_of(vres.get("tree")))
    row["verified"] = tree_ok(vres.get("tree"))
    row["ok"] = bool(row["verified"]) and row["launch_frame_md5"] == ref_md5
    return row


# -------------------------------------------------------------------- cohorts
def phase_m3(scratch, reps, ref, out, arm):
    """arm: 'off' | 'on' | 'on-wait'."""
    drop = True
    rows = []
    evicted_total = []
    for i in range(reps):
        tag = "m3-%s-%d" % (arm, i)
        row = {"rep": i, "arm": arm, "phase": "M3-cold-cache", "utc": utc(),
               "host": host_now()}
        ev = fadv_drop(cold_files())
        row["fadvise"] = ev
        before = sum(v.get("before") or 0 for v in ev.values())
        after = sum(v.get("after") or 0 for v in ev.values())
        row["evicted_before_bytes"], row["evicted_after_bytes"] = before, after
        evicted_total.append(before)
        t0, t0_utc = time.perf_counter(), utc()
        proc, sock, sock_ms, err = start_daemon(scratch, tag, no_prewarm=(arm == "off"))
        row["daemon_start_ms"] = sock_ms
        row["rss_ready_kb"] = rss_kb(proc.pid)
        try:
            if arm == "on-wait":
                wait_ms, line = wait_prewarm(err)
                row["prewarm_wait_ms"] = wait_ms
                row["prewarm_log"] = line
                row["rss_after_prewarm_kb"] = rss_kb(proc.pid)
            d = Daemon(sock)
            base = {"session_id": "s_m3_%d" % i}
            launch_start_ms = (time.perf_counter() - t0) * 1000.0
            # The launch's own UTC base: after any prewarm wait, so
            # `frame_landed_ms` (frame.captured_at - this) stays poll-free.
            r = one_launch(d, base, ref["md5"], launch_start_ms, utc())
            row.update(r)
            row["m3_ms"] = (time.perf_counter() - t0) * 1000.0
            if arm == "on":
                # Let the background thread finish, then keep its own record.
                wait_ms, line = wait_prewarm(err)
                row["prewarm_wait_ms"] = wait_ms
                row["prewarm_log"] = line
                row["rss_after_prewarm_kb"] = rss_kb(proc.pid)
            if r.get("ok"):
                dms, dr = d.call("acos_destroy", {"base": base})
                row["destroy_wall_ms"] = dms
                row["destroy_clean"] = bool(((dr.get("envelope") or {}).get("result") or {})
                                            .get("teardown", {}).get("verified"))
            d.close()
        finally:
            stop_daemon(proc)
        rows.append(row)
        print("%-9s rep %2d daemon=%.1f launch=%.1f total=%.1f verified=%s prewarm=%s"
              % (arm, i, row["daemon_start_ms"], row.get("launch_wall_ms", -1),
                 row.get("m3_ms", -1), row.get("ok"),
                 (row.get("prewarm_log") or "-")[:70]), flush=True)
    payload = {"phase": "M3-cold-cache", "arm": arm, "rows": rows,
               "definition": "nothing running -> acos serve + acos_launch -> first frame "
                             "byte-identical to the verified count-0 reference",
               "page_cache_dropped": True, "utc": utc(),
               "evicted_bytes_mean": (sum(evicted_total) / len(evicted_total)) if evicted_total else 0}
    benchlib.write_json(str(out), payload)
    return payload


def phase_m4(scratch, reps, ref, out):
    proc, sock, sock_ms, err = start_daemon(scratch, "m4", no_prewarm=False)
    rows = []
    try:
        rss0 = rss_kb(proc.pid)
        wait_ms, line = wait_prewarm(err)
        rss1 = rss_kb(proc.pid)
        d = Daemon(sock)
        b = {"session_id": "s_warm0"}
        d.call("acos_launch", {"base": b, "apk": str(APK)})
        d.call("acos_destroy", {"base": b})
        for i in range(reps):
            base = {"session_id": "s_m4_%d" % i}
            row = {"rep": i, "phase": "M4", "host": host_now()}
            t0, t0_utc = time.perf_counter(), utc()
            r = one_launch(d, base, ref["md5"], 0.0, t0_utc)
            row.update(r)
            dms, dr = d.call("acos_destroy", {"base": base})
            row["destroy_wall_ms"] = dms
            row["destroy_clean"] = bool(((dr.get("envelope") or {}).get("result") or {})
                                        .get("teardown", {}).get("verified"))
            rows.append(row)
            print("M4 rep %2d launch=%.1f verified=%s" %
                  (i, row.get("launch_wall_ms", -1), row.get("ok")), flush=True)
        d.close()
    finally:
        stop_daemon(proc)
    payload = {"phase": "M4", "rows": rows, "daemon_start_ms": sock_ms,
               "rss_ready_kb": rss0, "rss_after_prewarm_kb": rss1,
               "prewarm_wait_ms": wait_ms, "prewarm_log": line,
               "definition": "daemon already up, app not running -> acos_launch -> "
                             "first frame byte-identical to the verified count-0 reference",
               "utc": utc()}
    benchlib.write_json(str(out), payload)
    return payload


def summarise(rows, key):
    return benchlib.stats([r.get(key) for r in rows if r.get(key) is not None])


def analyse(dir_):
    d = Path(dir_)
    ref = json.loads((d / "ref.json").read_text())
    out = {"ref": ref, "rows": {}}
    for name in ("m3-off", "m3-on", "m3-on-wait", "m4"):
        path = d / (name + ".json")
        if not path.exists():
            continue
        payload = json.loads(path.read_text())
        rows = payload["rows"]
        out["rows"][name] = {
            "n": len(rows),
            "verified": sum(1 for r in rows if r.get("ok")),
            "false_passes": sum(1 for r in rows
                                if r.get("ok") and r.get("launch_frame_md5") != ref["md5"]),
            "launch_wall_ms": summarise(rows, "launch_wall_ms"),
            "m3_ms": summarise(rows, "m3_ms"),
            "cold_frame_ms": summarise(rows, "cold_frame_ms"),
            "frame_landed_ms": summarise(rows, "frame_landed_ms"),
            "daemon_start_ms": summarise(rows, "daemon_start_ms"),
            "prewarm_wait_ms": summarise(rows, "prewarm_wait_ms"),
            "rss_ready_kb": summarise(rows, "rss_ready_kb"),
            "rss_after_prewarm_kb": summarise(rows, "rss_after_prewarm_kb"),
            "evicted_before_mean": payload.get("evicted_bytes_mean"),
            "evicted_after_mean": (sum(r.get("evicted_after_bytes") or 0 for r in rows) / len(rows))
            if rows else None,
            "prewarm_logs": sorted({r.get("prewarm_log") for r in rows if r.get("prewarm_log")}),
            "accepted_on": sorted({r.get("accepted_on") for r in rows if r.get("accepted_on")}),
        }
    if "m3-off" in out["rows"] and "m3-on" in out["rows"]:
        off = out["rows"]["m3-off"]["m3_ms"]["p50"] or 0.0
        on = out["rows"]["m3-on"]["m3_ms"]["p50"] or 0.0
        out["delta_ms"] = off - on
        out["delta_pct"] = (100.0 * (off - on) / off) if off else None
    benchlib.write_json(str(d / "summary.json"), out)
    print(json.dumps({k: v for k, v in out.items() if k != "rows"}, indent=1))
    for name, row in out["rows"].items():
        print("%-11s n=%2d verified=%2d p50=%.1f p95=%.1f daemon=%.2f %s"
              % (name, row["n"], row["verified"],
                 row["m3_ms"]["p50"] if row["m3_ms"]["n"] else
                 (row["launch_wall_ms"]["p50"] or 0.0),
                 row["m3_ms"]["p95"] if row["m3_ms"]["n"] else
                 (row["launch_wall_ms"]["p95"] or 0.0),
                 row["daemon_start_ms"]["p50"] or 0.0,
                 row["prewarm_logs"][:1]))
    return out


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("cmd", choices=["all", "analyze"])
    ap.add_argument("--m3", type=int, default=12)
    ap.add_argument("--m4", type=int, default=12)
    ap.add_argument("--out", default=".cache/p8-run")
    a = ap.parse_args()
    if a.cmd == "analyze":
        analyse(a.out)
        return

    scratch = REPO / a.out if not Path(a.out).is_absolute() else Path(a.out)
    scratch.mkdir(parents=True, exist_ok=True)
    meta = {"started": utc(), "bin": str(BIN), "apk": str(APK), "m3": a.m3, "m4": a.m4,
            "sha256": subprocess.run(["sha256sum", str(BIN)], capture_output=True,
                                     text=True).stdout.split()[0],
            "host": host_now()}
    print("reference frame ...", flush=True)
    ref = make_reference(scratch)
    benchlib.write_json(str(scratch / "ref.json"), ref)
    print("reference md5 %s (%d nodes, count=%r)" %
          (ref["md5"], ref["nodes"], ref["count_text"]), flush=True)

    for arm in ("off", "on", "on-wait"):
        phase_m3(scratch, a.m3, ref, scratch / ("m3-%s.json" % arm), arm)
    phase_m4(scratch, a.m4, ref, scratch / "m4.json")
    meta["finished"] = utc()
    benchlib.write_json(str(scratch / "meta.json"), meta)
    analyse(scratch)


if __name__ == "__main__":
    main()
