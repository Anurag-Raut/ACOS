#!/usr/bin/env python3
"""s3_scale.py -- S3 Stage-3 concurrency harness (compat corpus + Gaten).

Two modes, both driven over the real daemon transport (`acos serve --engine art`,
newline JSON on a Unix socket) and both reusing the bench's own Serve/sampler
code instead of a second implementation:

  --mode compat   the 42-APK K1 corpus, `--n` apps at a time: launch -> first
                  frame -> fast tree, then destroy. Records first-frame rate,
                  launch/tree p50/p90, failure code per app and per-session
                  memory, so the N=5/N=10 funnel is directly comparable with
                  K1/K3's N=1 ladder.
  --mode gaten    `--n` Gaten sessions at once: launch (fast settle, no strict
                  assert -- the welcome screen never strictly settles) -> first
                  frame -> fast tree. Sessions are held together for the memory
                  record, then destroyed; session-exit.json is scanned for
                  native_crash.

Every guest runs inside the caller's one
`tools/art-runtime/run-exclusive.sh --task S3 --exclusive -- tools/guarded.sh --mem 24G`
acquisition.  Scratch is the caller's `.cache/s3/**`; nothing is written to
/tmp and the corpus (read-only) is never modified.

Usage:
  tools/bench/s3_scale.py --mode compat --n 5 --out R.json \
      --scratch .cache/s3/compat-n5 --animations on
  tools/bench/s3_scale.py --mode gaten --n 10 --out R.json \
      --scratch .cache/s3/gaten-n10
"""

from __future__ import annotations

import argparse
import glob
import json
import os
import shutil
import statistics
import sys
import threading
import time
import traceback
from pathlib import Path

HERE = Path(__file__).resolve().parent
REPO = HERE.parent.parent
sys.path.insert(0, str(REPO / "tools"))
sys.path.insert(0, str(REPO / "tools/art-runtime"))
sys.path.insert(0, str(HERE))

import benchlib  # noqa: E402
import d2_warm  # noqa: E402
import agent_loop  # noqa: E402
import acos_bench as ab  # noqa: E402  (reuse memory_snapshot/tree_snapshot)

DEFAULT_MANIFEST = REPO / ".cache/corpus/k1/manifest.json"
DEFAULT_GATEN = Path(
    "/home/Qui/Projects/acos/.cache/gaten/gaten-dev-f344272-x86_64-release-unsigned.apk")

# Filled by main(); the mode functions read the one daemon/sampler of the run.
serve = None
sampler = None
captured = {}


def utc():
    return benchlib.utc_now()


def host_now():
    h = {**benchlib.read_meminfo(), **benchlib.read_loadavg(), "utc": utc()}
    h["noisy"] = bool((h.get("loadavg1") or 0) > 4.0
                      or (h.get("mem_available_gib") or 0) < 20.0)
    return h


def _ancestors(pid=None):
    """This process and its parents: their cmdlines carry the scratch path and
    must never be reported as leaked guests."""
    out = set()
    cur = int(pid or os.getpid())
    while cur and cur > 0 and cur not in out:
        out.add(cur)
        try:
            fields = Path("/proc/%d/stat" % cur).read_text().rsplit(") ", 1)[-1].split()
            cur = int(fields[1])
        except (OSError, ValueError, IndexError):
            break
    return out


def survivors(scratch):
    """Live processes naming this run's scratch, excluding our own ancestry."""
    skip = _ancestors()
    out = []
    for name in os.listdir("/proc"):
        if not name.isdigit():
            continue
        pid = int(name)
        if pid in skip:
            continue
        try:
            cmd = Path("/proc/%s/cmdline" % name).read_bytes().replace(
                b"\0", b" ").decode("utf-8", "replace")
        except OSError:
            continue
        if str(scratch) in cmd:
            try:
                comm = Path("/proc/%s/comm" % name).read_text().strip()
            except OSError:
                comm = "?"
            out.append({"pid": pid, "comm": comm, "cmd": cmd[:240]})
    return out


def count_nodes(tree):
    return sum(1 for _ in agent_loop.walk((tree or {}).get("nodes")))


def texts_of(tree, limit=12):
    out = []
    for node in agent_loop.walk((tree or {}).get("nodes")):
        value = node.get("text") or node.get("content_desc")
        if value:
            out.append(str(value))
        if len(out) >= limit:
            break
    return out


def error_row(env):
    err = env.get("error") or {}
    details = err.get("details") or {}
    return {
        "code": err.get("code"),
        "message": (err.get("message") or "")[:400],
        "stage": details.get("stage"),
        "guest_exit": details.get("guest_exit"),
        "guest_signal": details.get("guest_signal"),
        "guest_crash": (details.get("guest_crash") or "")[:600],
        "unsupported_api": details.get("api") or details.get("symbol"),
        "session_exit": details.get("session_exit"),
    }


class PssSampler(benchlib.ChurnSampler):
    """ChurnSampler + a 1 Hz sum-Pss snapshot of the live serve tree.

    S3c showed the legacy WarmSampler missed short-lived guests (the compat
    corpus is almost entirely short-lived), so the compat/Gaten memory record
    now uses the creation-time discovery and coverage accounting of S3c.
    """

    def __init__(self, job, sampler_pid, serve_pid, interval=0.1, **kw):
        super().__init__(job, sampler_pid=sampler_pid, interval=interval, **kw)
        self.serve_pid = int(serve_pid)
        self.pss_rows = []

    def sample_once(self):
        row = super().sample_once()
        if self.ticks % 10 == 1:
            try:
                snap = ab.memory_snapshot(self.serve_pid, self)
                snap["t"] = row["t"]
                self.pss_rows.append(snap)
            except Exception as exc:  # noqa: BLE001
                self.errors.append("pss: %s: %s" % (type(exc).__name__, exc))
        return row


def memory_record(sampler, n, window=None, expected_guests=None):
    rows = [r for r in (sampler.pss_rows if sampler else [])
            if window is None or window[0] <= r["t"] <= window[1]]
    samples = [s for s in (sampler.samples if sampler else [])
               if window is None or window[0] <= s["t"] <= window[1]]
    summary = {}
    if sampler is not None and window:
        summary = sampler.summary(window[0], window[1], sessions=n,
                                  expected_guests=expected_guests)
    out = {"samples": len(rows),
           "coverage": summary.get("coverage"),
           "guests_observed": summary.get("guests_observed"),
           "guests_expected": summary.get("guests_expected"),
           "samples_per_guest": summary.get("samples_per_guest"),
           "sampler_ticks": len(samples)}
    if rows:
        pss = [r["sum_pss_mib"] for r in rows if r.get("sum_pss_mib") is not None]
        spss = [r["session_pss_mib"] for r in rows
                if r.get("session_pss_mib") is not None]
        if pss:
            out["sum_pss_mib"] = {"median": round(statistics.median(pss), 3),
                                  "max": round(max(pss), 3)}
        if spss:
            out["session_pss_mib"] = {"median": round(statistics.median(spss), 3),
                                      "max": round(max(spss), 3)}
            out["session_pss_per_session_mib"] = round(
                statistics.median(spss) / n, 3)
    if samples:
        net = [s["net_anon_shmem_mib"] for s in samples
               if s.get("net_anon_shmem_mib") is not None]
        cur = [s["job_memory_current_mib"] for s in samples
               if s.get("job_memory_current_mib") is not None]
        if net:
            out["net_anon_shmem_mib"] = {"median": round(statistics.median(net), 3),
                                         "max": round(max(net), 3)}
            out["net_anon_shmem_per_session_mib"] = round(
                statistics.median(net) / n, 3)
        if cur:
            out["cgroup_memory_current_mib"] = {"max": round(max(cur), 3)}
    # S3c: report the churn-safe GiB-min numbers and their coverage together.
    for key in ("net_anon_shmem_gib_min", "guest_anon_shmem_gib_min",
                "job_gib_min"):
        if summary.get(key) is not None:
            out[key] = summary[key]
    out["coverage_rule"] = ("guest scopes observed / one per acos_launch "
                            "(no reset in this harness); coverage is an upper "
                            "bound when a scope is created and removed between ticks")
    return out


def launch_tree(session, base, apk, timeout, settle, arm, extra=None):
    """launch -> first frame -> fast tree, with all walls and error classes."""
    row = {"ok": False}
    t0 = time.time()
    args = {"base": base, "apk": str(apk), "settle": settle}
    if arm:
        args["animations"] = arm
    if extra:
        args.update(extra)
    try:
        r = session.call("acos_launch", args)
    except Exception as exc:  # noqa: BLE001 -- a dead socket is a failure row
        row.update({"launch_ok": False, "launch_ms": round((time.time() - t0) * 1e3, 1),
                    "error": {"code": "HARNESS_" + type(exc).__name__.upper(),
                              "message": str(exc)[:400]}})
        return row
    env = r["envelope"]
    value = env.get("result") or {}
    row.update({
        "launch_ok": bool(env.get("ok")),
        "launch_ms": round(r["wall_ms"], 3),
        "status": env.get("status"),
        "first_frame": bool(env.get("ok") and (value.get("frame") is not None
                                               or value.get("tree") is not None)),
        "stage_reached": value.get("stage_reached"),
        "activity": value.get("activity"),
        "package": value.get("package")
        or ((value.get("tree") or {}).get("package")),
        "tree_nodes_launch": value.get("tree_node_count"),
        "session_start_ms": (value.get("timing") or {}).get("session_start_ms"),
        "error": error_row(env) if not env.get("ok") else None,
    })
    if not env.get("ok"):
        return row
    try:
        t = time.time()
        tr = session.call("acos_observe_tree",
                          {"base": base, "settle": settle, "max_nodes": 3000})
        tenv = tr["envelope"]
        tree = (tenv.get("result") or {}).get("tree")
        row.update({
            "tree_ok": bool(tenv.get("ok")),
            "tree_ms": round(tr["wall_ms"], 3),
            "tree_nodes": count_nodes(tree),
            "texts": texts_of(tree),
            "tree_error": error_row(tenv) if not tenv.get("ok") else None,
        })
    except Exception as exc:  # noqa: BLE001
        row.update({"tree_ok": False, "tree_ms": round((time.time() - t0) * 1e3, 1),
                    "tree_error": {"code": "HARNESS_" + type(exc).__name__.upper(),
                                   "message": str(exc)[:400]}})
    row["ok"] = bool(row.get("first_frame") and row.get("tree_ok"))
    return row


def destroy(session, base, rows):
    try:
        r = session.call("acos_destroy", {"base": base})
    except Exception as exc:  # noqa: BLE001
        rows.append({"destroy_error": "%s: %s" % (type(exc).__name__, exc)})
        return
    env = r["envelope"]
    rows.append({"destroy_ok": bool(env.get("ok")),
                 "destroy_ms": round(r["wall_ms"], 1),
                 "teardown_ms": ((env.get("result") or {}).get("teardown") or {}
                                 ).get("teardown_ms")})


def stats_ms(values):
    vals = [v for v in values if v is not None]
    if not vals:
        return {"n": 0}
    return {
        "n": len(vals),
        "p50": round(benchlib.percentile(vals, 50), 3),
        "p90": round(benchlib.percentile(vals, 90), 3),
        "max": round(max(vals), 3),
    }


def failure_classes(rows):
    counts = {}
    for row in rows:
        if row.get("ok"):
            continue
        err = row.get("error") or row.get("tree_error") or {}
        code = err.get("code") or "NO_FIRST_FRAME"
        counts[code] = counts.get(code, 0) + 1
    return dict(sorted(counts.items(), key=lambda kv: -kv[1]))


def find_session_exits(root, since):
    """Every session-exit.json written under the run's artifact root."""
    out = []
    for path in sorted(Path(root).rglob("session-exit.json")):
        try:
            if path.stat().st_mtime < since - 5:
                continue
            doc = json.loads(path.read_text())
        except (OSError, ValueError):
            continue
        out.append({
            "path": str(path.relative_to(root)),
            "outcome": doc.get("outcome"),
            "code": doc.get("code"),
            "stage": doc.get("stage"),
            "guest_exit": doc.get("guest_exit"),
            "guest_signal": doc.get("guest_signal"),
            "native_crash": doc.get("native_crash"),
            "native_crash_path": doc.get("native_crash_path"),
            "guest_crash": (doc.get("guest_crash") or "")[:300],
        })
    return out


def compat_mode(args):
    manifest = json.loads(Path(args.manifest).read_text())
    apps = sorted(manifest["apps"], key=lambda a: a["package"])
    corpus = Path(args.manifest).parent / "apks"
    tasks = [{"package": a["package"], "apk": str(corpus / a["name"])}
             for a in apps]
    missing = [t for t in tasks if not Path(t["apk"]).is_file()]
    tasks = [t for t in tasks if Path(t["apk"]).is_file()]
    queue = list(reversed(tasks))
    qlock = threading.Lock()
    rows = []
    rlock = threading.Lock()
    errors = []
    session_pids = {}

    def worker(slot):
        d = serve.client()
        d.timeout = args.timeout
        d.sock.settimeout(args.timeout)
        try:
            while True:
                with qlock:
                    if not queue:
                        return
                    task = queue.pop()
                # one unique session id per (slot, app) so nothing is reused
                sid = "s3c_%d_%s" % (slot, task["package"].replace(".", "-")[:40])
                base = {"request_id": "s3c-%d-%s" % (slot, task["package"]),
                        "session_id": sid}
                row = launch_tree(d, base, task["apk"], args.timeout,
                                  args.settle, args.animations)
                row.update({"package": task["package"], "slot": slot,
                            "session_id": sid})
                d_rows = []
                destroy(d, base, d_rows)
                row["destroy"] = d_rows
                with rlock:
                    rows.append(row)
        except Exception as exc:  # noqa: BLE001
            with rlock:
                errors.append({"slot": slot, "error": "%s: %s" % (
                    type(exc).__name__, exc), "trace": traceback.format_exc()[-1200:]})
        finally:
            try:
                d.close()
            except OSError:
                pass

    threads = [threading.Thread(target=worker, args=(i,), daemon=True)
               for i in range(args.n)]
    sessions_start = time.time()
    for th in threads:
        th.start()
    for th in threads:
        th.join()
    return {
        "mode": "compat", "tasks_total": len(tasks), "missing_apks": missing,
        "rows": sorted(rows, key=lambda r: r["package"]), "errors": errors,
        "sessions_start_utc": sessions_start,
    }


def gaten_mode(args):
    apk = Path(args.apk)
    rows = []
    errors = []
    sessions = [serve.client() for _ in range(args.n)]
    for d in sessions:
        d.timeout = args.timeout
        d.sock.settimeout(args.timeout)
    barrier = threading.Barrier(args.n)

    def worker(i):
        d = sessions[i]
        base = {"request_id": "s3g-%d" % i, "session_id": "s3g_%d" % i}
        try:
            barrier.wait(timeout=120)
            row = launch_tree(d, base, apk, args.timeout, args.settle,
                              args.animations, {"clear_state": True})
            row.update({"slot": i, "session_id": base["session_id"]})
            if row.get("first_frame") and args.tree_delay > 0:
                time.sleep(args.tree_delay)
                try:
                    tr = d.call("acos_observe_tree",
                                {"base": base, "settle": args.settle,
                                 "max_nodes": 3000})
                    tenv = tr["envelope"]
                    tree = (tenv.get("result") or {}).get("tree")
                    row.update({
                        "tree_after_delay_ms": round(tr["wall_ms"], 3),
                        "tree_after_delay_ok": bool(tenv.get("ok")),
                        "tree_after_delay_nodes": count_nodes(tree),
                        "texts_after_delay": texts_of(tree),
                    })
                except Exception as exc:  # noqa: BLE001
                    row["tree_after_delay_error"] = "%s: %s" % (
                        type(exc).__name__, exc)
            with rlock:
                rows.append(row)
        except Exception as exc:  # noqa: BLE001
            with rlock:
                errors.append({"slot": i, "error": "%s: %s" % (
                    type(exc).__name__, exc)})

    rlock = threading.Lock()
    threads = [threading.Thread(target=worker, args=(i,), daemon=True)
               for i in range(args.n)]
    t_cohort = time.time()
    for th in threads:
        th.start()
    for th in threads:
        th.join()
    cohort_wall = round(time.time() - t_cohort, 3)
    # Hold every session together for the memory record (per-session footprint
    # at N, not a transient launch peak).
    time.sleep(args.hold)
    mem = memory_record(sampler, args.n, window=(t_cohort - 1, time.time() + 1))
    destroy_rows = []
    for i, d in enumerate(sessions):
        destroy(d, {"request_id": "s3g-d-%d" % i,
                    "session_id": "s3g_%d" % i}, destroy_rows)
        try:
            d.close()
        except OSError:
            pass
    return {
        "mode": "gaten", "apk": str(apk), "cohort_wall_s": cohort_wall,
        "hold_s": args.hold, "rows": sorted(rows, key=lambda r: r.get("slot", -1)),
        "destroy": destroy_rows, "errors": errors,
        "memory": mem,
    }


def summarize(args, payload, sampler, t0, t1):
    rows = payload.get("rows") or []
    ok = [r for r in rows if r.get("ok")]
    first = [r for r in rows if r.get("first_frame")]
    # A failed launch/tree must never contribute a latency sample.
    launch_ms = [r.get("launch_ms") for r in ok]
    tree_ms = [r.get("tree_ms") for r in ok]
    out = {
        "n": args.n,
        "apps": len(rows),
        "first_frame": len(first),
        "first_frame_rate": round(len(first) / len(rows), 4) if rows else None,
        "tree_ok": len(ok),
        "tree_rate": round(len(ok) / len(rows), 4) if rows else None,
        "launch_ms": stats_ms(launch_ms),
        "tree_ms": stats_ms(tree_ms),
        "tree_nodes": stats_ms([r.get("tree_nodes") for r in ok]),
        "tree_nodes_after_delay": stats_ms(
            [r.get("tree_after_delay_nodes") for r in ok]),
        "failure_classes": failure_classes(rows),
        "host_cpu": ab.host_cpu_delta(captured.get("cpu0"), ab.host_cpu_times()),
        "memory": memory_record(
            sampler, args.n, window=(t0 - 1, t1 + 1),
            expected_guests=sum(1 for r in rows if r.get("launch_ok"))),
        "per_session_anon_shmem_mib": None,
    }
    mem = out["memory"]
    out["coverage"] = mem.get("coverage")
    out["guests_observed"] = mem.get("guests_observed")
    out["guests_expected"] = mem.get("guests_expected")
    if mem.get("net_anon_shmem_mib"):
        out["per_session_anon_shmem_mib"] = round(
            mem["net_anon_shmem_mib"]["median"] / args.n, 3)
    return out


def main(argv=None):
    p = argparse.ArgumentParser(description=__doc__,
                                formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("--mode", choices=["compat", "gaten"], required=True)
    p.add_argument("--n", type=int, required=True)
    p.add_argument("--out", required=True)
    p.add_argument("--scratch", required=True)
    p.add_argument("--acos", default=str(REPO / "target/release/acos"))
    p.add_argument("--animations", choices=["on", "off"], default="on")
    p.add_argument("--settle", choices=["fast", "strict"], default="fast")
    p.add_argument("--timeout", type=float, default=90.0,
                   help="per-request socket timeout (first-frame budget)")
    p.add_argument("--manifest", default=str(DEFAULT_MANIFEST))
    p.add_argument("--apk", default=str(DEFAULT_GATEN))
    p.add_argument("--hold", type=float, default=5.0,
                   help="seconds all Gaten sessions are held before the memory read")
    p.add_argument("--tree-delay", type=float, default=2.0,
                   help="Gaten: seconds between first frame and the second tree read "
                        "(the immediate read often sees only the RN root view)")
    p.add_argument("--keep-scratch", action="store_true")
    args = p.parse_args(argv)
    global serve, sampler, captured
    if not Path(args.acos).is_file():
        raise SystemExit("s3_scale: missing binary %s" % args.acos)
    scratch = Path(args.scratch)
    if scratch.exists():
        shutil.rmtree(scratch)
    scratch.mkdir(parents=True)
    os.environ.setdefault(
        "ACOS_FONT_DIR", "/home/Qui/Projects/acos/.cache/boot/android-34/fonts")
    os.environ.pop("ACOS_BOOT_DIR", None)
    if not os.environ.get("ACOS_ART_DATA"):
        os.environ["ACOS_ART_DATA"] = str(scratch / "art-data")
    captured = {"cpu0": ab.host_cpu_times()}
    serve = d2_warm.Serve(args.acos, scratch, engine="art",
                          max_sessions=max(args.n + 2, 10))
    job = benchlib.cgroup_path(serve.proc.pid)
    sampler = None
    if job:
        sampler = PssSampler(
            job, os.getpid(), serve.proc.pid, interval=0.1,
            scope_parent=Path(job).parent,
            artifacts=[str(scratch / "sessions")], root_pid=os.getpid())
        sampler.start()
    doc = {
        "schema": "acos-s3-scale/1", "utc": utc(), "mode": args.mode,
        "n": args.n, "animations": args.animations, "settle": args.settle,
        "acos": args.acos, "binary_sha256": benchlib.sha256_file(args.acos),
        "host_before": host_now(), "job_cgroup": str(job) if job else None,
        "scratch": str(scratch),
    }
    t0 = time.time()
    try:
        payload = compat_mode(args) if args.mode == "compat" else gaten_mode(args)
        doc.update(payload)
    except Exception as exc:  # noqa: BLE001
        doc["errors"] = [{"error": "%s: %s" % (type(exc).__name__, exc),
                          "trace": traceback.format_exc()[-1600:]}]
        payload = {}
    t1 = time.time()
    doc["wall_s"] = round(t1 - t0, 3)
    doc["host_after"] = host_now()
    doc["sampler_errors"] = sampler.errors if sampler else []
    doc["summary"] = summarize(args, doc, sampler, t0, t1)
    doc["session_exits"] = find_session_exits(scratch, t0)
    destroy_failures = []
    for row in doc.get("rows") or []:
        for drow in row.get("destroy") or []:
            if drow.get("destroy_error") or not drow.get("destroy_ok", True):
                destroy_failures.append({"session": row.get("session_id"),
                                         **drow})
    for drow in doc.get("destroy") or []:
        if drow.get("destroy_error") or not drow.get("destroy_ok", True):
            destroy_failures.append(drow)
    doc["destroy_failures"] = destroy_failures
    missing = doc.get("missing_apks") or []
    harness_errors = list(doc.get("errors") or [])
    if missing:
        harness_errors.append("missing APK(s): %s" % (missing,))
    if doc.get("sampler_errors"):
        harness_errors.append("sampler errors: %s" % (doc["sampler_errors"][:3],))
    doc["harness_errors"] = harness_errors
    rows = doc.get("rows") or []
    first_frame = (doc.get("summary") or {}).get("first_frame") or 0
    doc["ok"] = bool(not harness_errors and rows and first_frame > 0)
    # A cohort that launched nothing, or a broken/missing fixture, is a failed
    # harness run -- not an empty-but-successful table.
    doc["harness_ok"] = doc["ok"]
    if sampler:
        sampler.stop()
    try:
        doc["serve_rc"] = serve.close()
    except Exception:  # noqa: BLE001
        doc["serve_rc"] = None
    # A leak check, not a self-check: run it only after the daemon is gone, so
    # the harness's own serve process can never be reported as a survivor.
    doc["survivors"] = survivors(scratch)
    out = Path(args.out)
    out.parent.mkdir(parents=True, exist_ok=True)
    benchlib.write_json(str(out), doc)
    print("s3-scale: mode=%s n=%d first_frame=%s/%s tree=%s/%s p50=%s ms -> %s"
          % (args.mode, args.n, doc["summary"]["first_frame"], doc["summary"]["apps"],
             doc["summary"]["tree_ok"], doc["summary"]["apps"],
             (doc["summary"]["launch_ms"] or {}).get("p50"), out))
    if not args.keep_scratch and scratch.exists():
        shutil.rmtree(scratch, ignore_errors=True)
    return 0 if doc.get("ok") else 1


if __name__ == "__main__":
    sys.exit(main())
