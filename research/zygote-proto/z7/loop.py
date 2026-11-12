#!/usr/bin/env python3
"""Z7 agent loop: launch -> tap/type -> verify -> reset (relaunch), N concurrent.

The prototype's zygote cohort forks a fixed set of children once (Z4 seam), so a
per-session relaunch inside one zygote is not available. To keep the four arms
comparable, every arm runs the *same* round protocol:

  round = launch N fresh sessions (a: N exec'd guests; c: one zygote cohort)
        + one verified task on every session
        + reset = close the whole cohort and launch a fresh one

Every task therefore runs on a freshly launched child (the P5 pattern: reset +
relaunch per task), which is exactly the regime Z6's 10 s settling window made
unmeasurable. The report states the latency from a session's own launch to its
verified result, the tasks/min, and the cgroup memory integral (P5's GiB-min
definition: `tools/benchlib.py::sample_integral` over the guarded job cgroup's
`memory.current`, sampled every 100 ms).

`--cycles` replaces the time box with a fixed number of rounds (the 20-cycle
teardown/leak test); per round it records the zygote's pid/threads/fds/RSS and
every launcher's exit code.
"""
import argparse
import json
import os
import statistics
import sys
import time
from pathlib import Path

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
sys.path.insert(0, os.path.join(HERE, "..", "..", "..", "tools"))
sys.path.insert(0, os.path.join(HERE, "..", "..", "..", "tools", "art-runtime"))
import common as C  # noqa: E402
import drive as z4  # noqa: E402
import benchlib  # noqa: E402
import d2_warm  # noqa: E402


class PssSampler:
    """Whole-tree `smaps_rollup` Pss every 200 ms (attributable resident memory).

    The guarded cgroup's `memory.current` only sees processes still inside that
    scope; every `acos-art` launch creates its own systemd transient scope as a
    *sibling*, so the per-job cgroup misses the guests (recorded in the report).
    This sampler walks the harness's own process tree, so it sees every launcher,
    bwrap, zygote and guest, and sums the same `Pss` the lifetime cells use.
    """
    def __init__(self, root_pid, interval=0.2):
        import threading
        self._threading = threading
        self.root_pid = int(root_pid)
        self.interval = interval
        self.samples = []
        self._stop = threading.Event()
        self._t = None

    def start(self):
        def run():
            while not self._stop.is_set():
                t0 = time.time()
                total = 0
                n = 0
                for pid in z4.subtree(self.root_pid):
                    if pid == self.root_pid:
                        continue
                    roll = z4.read_rollup(pid)
                    if roll is None:
                        continue
                    total += roll.get("Pss", 0)
                    n += 1
                self.samples.append({"t": t0, "pss_kib": total, "processes": n})
                left = self.interval - (time.time() - t0)
                if left > 0:
                    self._stop.wait(left)
        self._t = self._threading.Thread(target=run, daemon=True)
        self._t.start()

    def stop(self):
        self._stop.set()
        if self._t:
            self._t.join(timeout=3)


def stats(vals):
    vals = sorted(v for v in vals if v is not None)
    if not vals:
        return None
    def pct(p):
        if len(vals) == 1:
            return vals[0]
        i = min(len(vals) - 1, int(round((len(vals) - 1) * p)))
        return vals[i]
    return {"n": len(vals), "p50": round(pct(0.50), 1),
            "p95": round(pct(0.95), 1), "min": round(vals[0], 1),
            "max": round(vals[-1], 1),
            "mean": round(sum(vals) / len(vals), 1)}


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--arm", required=True, choices=C.ARMS)
    ap.add_argument("--n", type=int, required=True)
    ap.add_argument("--out", required=True)
    ap.add_argument("--json", required=True)
    ap.add_argument("--minutes", type=float, default=5.0)
    ap.add_argument("--cycles", type=int, default=0,
                    help="fixed number of rounds instead of the time box")
    ap.add_argument("--app", default="counter", choices=["counter", "form-validation"])
    ap.add_argument("--close-order", default="owner-first",
                    choices=["owner-first", "children-first"])
    ap.add_argument("--close-bound", type=float, default=5.0,
                    help="seconds a close may take before the launcher is SIGKILLed")
    ap.add_argument("--keep-leftovers", action="store_true",
                    help="do not kill the cohort's surviving processes after close")
    args = ap.parse_args()
    profile = C.app_profile(args.app)
    root = os.path.abspath(args.out)
    os.makedirs(root, exist_ok=True)
    job_cg = benchlib.cgroup_path(os.getpid())
    sampler = d2_warm.WarmSampler(Path(job_cg), os.getpid(), interval=0.1) \
        if job_cg else None
    if sampler:
        sampler.start()
    pss_sampler = PssSampler(os.getpid())
    pss_sampler.start()
    report = {
        "schema": "z7-loop/1", "arm": args.arm, "mode": C.MODES[args.arm],
        "n": args.n, "app": args.app, "minutes": args.minutes,
        "cycles_requested": args.cycles, "close_order": args.close_order,
        "job_cgroup": str(job_cg) if job_cg else None,
        "started_utc": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
        "art_bin": C.ART, "rounds": [], "failures": [], "tasks": [],
    }
    wall0 = time.time()
    round_no = 0
    try:
        while True:
            if args.cycles:
                if round_no >= args.cycles:
                    break
            elif time.time() - wall0 >= args.minutes * 60.0:
                break
            rno = round_no
            round_no += 1
            plan = os.path.join(root, "rounds", str(rno), "sessions")
            out = os.path.join(root, "rounds", str(rno))
            rec = {"round": rno, "started_s": round(time.time() - wall0, 2)}
            procs, sessions, t0, overlay = C.spawn_cohort(args.arm, args.n, out, plan,
                                                          args.app)
            rec["knobs"] = overlay
            report.setdefault("knobs", overlay)
            try:
                try:
                    ready = C.read_all_ready(sessions, t0)
                except BaseException as e:  # one bad launch must not end the run
                    rec["ready_error"] = repr(e)
                    report["failures"].append(f"round {rno}: ready error {e!r}")
                    ready = {}
                rec["all_ready_ms"] = round((time.perf_counter() - t0) * 1000.0, 1)
                rec["ready_ms"] = {str(k): v["t_ready_ms"] for k, v in ready.items()}
                bad = {k: v["reply"] for k, v in ready.items()
                       if not v["reply"].get("ok")}
                if bad:
                    report["failures"].append(f"round {rno}: ready failures {bad}")
                # per-cycle process facts before the tasks start
                zpid = (C.find_zygote_host_pid(procs[0].pid)
                        if C.MODES[args.arm] == "zygote" else None)
                rec["zygote_before"] = C.proc_stats(zpid) if zpid else None
                rec["zygote_pid"] = zpid
                rec["launcher0_before"] = C.proc_stats(procs[0].pid)
                for k in sorted(sessions):
                    if k not in ready or not ready[k]["reply"].get("ok"):
                        continue
                    rec.setdefault("session", {})[str(k)] = {
                        "ready_ms": ready[k]["t_ready_ms"]}
                    try:
                        ok, task = C.verify_task(sessions[k], profile, k,
                                                 rec["session"][str(k)])
                    except BaseException as e:
                        task = {"tag": k, "error": repr(e), "verified": False}
                        ok = False
                    task["round"] = rno
                    task["task_latency_ms"] = round(
                        (time.perf_counter() - t0) * 1000.0, 1)
                    task["verified"] = ok
                    report["tasks"].append(task)
                    if not ok:
                        report["failures"].append(
                            f"round {rno} session {k}: task not verified")
                rec["zygote_after_tasks"] = (
                    C.proc_stats(zpid) if zpid else None)
            finally:
                order = sorted(sessions)
                if C.MODES[args.arm] == "zygote" and args.close_order == "owner-first":
                    order = [0] + [k for k in order if k != 0]
                rec["close"] = C.close_cohort(sessions, procs, order=order,
                                              bound=args.close_bound)
                rec["zygote_alive_after_close"] = (
                    bool(rec.get("zygote_pid"))
                    and os.path.exists(f"/proc/{rec['zygote_pid']}"))
                if rec.get("zygote_pid"):
                    rec["zygote_after_close"] = C.proc_stats(rec["zygote_pid"])
                rec["exits_clean"] = all(v.get("exit") == 0 for v in rec["close"].values())
                # Reset semantics for the next round: a launcher/zygote that did
                # not exit after its close is killed (recorded, not hidden). The
                # teardown test (cycles) reports the same fact before this runs.
                killed = []
                if not args.keep_leftovers:
                    for k, p in procs.items():
                        if p.poll() is None:
                            killed += C.kill_tree(p.pid)
                    zp = rec.get("zygote_pid")
                    if zp and os.path.exists(f"/proc/{zp}"):
                        killed += C.kill_tree(zp)
                    time.sleep(0.2)
                rec["leftovers_killed"] = sorted(set(killed))
                rec["cohort_gone_after_reset"] = all(
                    p.poll() is not None for p in procs.values()) and not (
                    rec.get("zygote_pid")
                    and os.path.exists(f"/proc/{rec['zygote_pid']}"))
                rec["finished_s"] = round(time.time() - wall0, 2)
                report["rounds"].append(rec)
                print(f"[{args.arm}-n{args.n}] round {rno} ready={rec.get('all_ready_ms')}ms "
                      f"tasks={sum(1 for t in report['tasks'] if t.get('round')==rno)} "
                      f"exits={ {k: v.get('exit') for k, v in rec['close'].items()} } "
                      f"zygote_alive={rec.get('zygote_alive_after_close')}", flush=True)
    finally:
        if sampler:
            sampler.stop()
        pss_sampler.stop()
        wall = time.time() - wall0
        verified = [t for t in report["tasks"] if t.get("verified")]
        report["wall_s"] = round(wall, 2)
        report["verified_tasks"] = len(verified)
        report["false_passes"] = sum(1 for t in report["tasks"]
                                     if t.get("verified") and not t.get("tap_ok"))
        report["tasks_per_min"] = round(len(verified) / (wall / 60.0), 3)
        report["task_latency_ms"] = stats([t.get("task_latency_ms") for t in verified])
        # P5's denominator: memory.current integral over the guarded job cgroup.
        srows = []
        if sampler:
            for s in sampler.samples:
                if s.get("job_memory_current_mib") is None:
                    continue
                srows.append({"t": s["t"],
                              "bytes": s["job_memory_current_mib"] * 1024.0 * 1024.0})
        gib_min = benchlib.sample_integral(srows) if srows else None
        net_rows = []
        if sampler:
            for s in sampler.samples:
                if s.get("net_anon_shmem_mib") is None:
                    continue
                net_rows.append({"t": s["t"],
                                 "bytes": s["net_anon_shmem_mib"] * 1024.0 * 1024.0})
        net_gib_min = benchlib.sample_integral(net_rows) if net_rows else None
        report["sampler"] = {
            "samples": len(srows),
            "memory_current_peak_mib": (round(benchlib.sample_peak(
                [{"bytes": r["bytes"]} for r in srows]) / 1024.0 / 1024.0, 1)
                if srows else None),
            "gib_min": round(gib_min, 5) if gib_min else None,
            "tasks_per_gib_min": (round(len(verified) / gib_min, 1)
                                  if gib_min else None),
            "net_anon_shmem_peak_mib": (round(benchlib.sample_peak(
                [{"bytes": r["bytes"]} for r in net_rows]) / 1024.0 / 1024.0, 1)
                if net_rows else None),
            "net_anon_shmem_gib_min": round(net_gib_min, 5) if net_gib_min else None,
            "tasks_per_gib_min_net": (round(len(verified) / net_gib_min, 1)
                                      if net_gib_min else None),
            "errors": sampler.errors if sampler else None,
        }
        pss_rows = [{"t": s["t"], "bytes": s["pss_kib"] * 1024.0}
                    for s in pss_sampler.samples]
        pss_gib_min = benchlib.sample_integral(pss_rows) if pss_rows else None
        report["pss_sampler"] = {
            "samples": len(pss_rows),
            "pss_peak_mib": (round(max((r["bytes"] for r in pss_rows)) / 1024.0 / 1024.0, 1)
                             if pss_rows else None),
            "gib_min": round(pss_gib_min, 5) if pss_gib_min else None,
            "tasks_per_gib_min": (round(len(verified) / pss_gib_min, 1)
                                  if pss_gib_min else None),
        }
        report["verdict"] = "pass" if not report["failures"] else "fail"
        C.write_json(args.json, report)
        print(f"[{args.arm}-n{args.n}] verdict={report['verdict']} "
              f"tasks={len(verified)} wall={wall:.1f}s "
              f"tasks/GiB-min={report['sampler']['tasks_per_gib_min']} -> {args.json}",
              flush=True)


if __name__ == "__main__":
    main()
