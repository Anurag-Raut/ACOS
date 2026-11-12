#!/usr/bin/env python3
"""P5: time one full agent verification loop on a held `acos serve` ART session.

Loop per task (a held session, counter.apk):

    launch -> inspect (tree) -> tap(inc) -> observe (tree+text, then PNG)
           -> assert (fresh tree, count == 1) -> reset (fresh guest)

Each request is one client round trip over the daemon's real transport (newline
JSON on a Unix socket). Served by `acos serve`; the server and the metric code
are imported from tools/art-runtime/d2_warm.py and tools/benchlib.py, never
duplicated. Per-request wall time comes from the client; the launcher/serve/
engine phase traces are the existing env-gated hooks (P3_TRACE, DM5_TRACE,
DM6_RESET_TRACE) and are inert unless set. No production code is changed.

Usage:
  python3 -I p5_loop.py --acos .cache/p5-target/release/acos \
      --out results/art-runtime/evidence/P5/loop-n1.json --n 1 --loops 22 --warmup 1

Scratch: .cache/p5-<tag>/ (never /tmp).
"""

from __future__ import annotations

import argparse
import json
import os
import statistics
import sys
import threading
import time
from pathlib import Path

REPO = Path("/home/Qui/Projects/acos")
sys.path.insert(0, str(REPO / "tools"))
sys.path.insert(0, str(REPO / "tools/art-runtime"))

import benchlib  # noqa: E402
import d2_warm  # noqa: E402
from d2_warm import Serve, WarmSampler, descendants  # noqa: E402
import agent_loop  # noqa: E402

APK = REPO / "fixtures/build/apps/counter.apk"
TAP_ID = "inc"
ASSERT_ID = "count"
PRE_TEXT = "0"
POST_TEXT = "1"

IO_FIELDS = ("rchar", "wchar", "syscr", "syscw", "read_bytes", "write_bytes")


def walk(nodes):
    for node in nodes or []:
        if not isinstance(node, dict):
            continue
        yield node
        yield from walk(node.get("children"))


def find_node(tree, resource_id):
    for node in walk((tree or {}).get("nodes")):
        rid = node.get("resource_id") or ""
        if rid == resource_id or rid.endswith(":id/" + resource_id):
            return node
    return None


def node_text(node):
    if not node:
        return ""
    value = node.get("text")
    if value is None:
        value = node.get("content_desc")
    return "" if value is None else str(value)


def tree_of(value):
    return (value or {}).get("tree") if isinstance(value, dict) else None


def count_nodes(tree):
    return sum(1 for _ in walk((tree or {}).get("nodes")))


def io_snapshot(pids):
    out = dict.fromkeys(IO_FIELDS, 0)
    for pid in pids:
        try:
            for line in Path("/proc/%d/io" % pid).read_text().splitlines():
                key, _, val = line.partition(":")
                if key in out:
                    out[key] += int(val)
        except (OSError, ValueError):
            continue
    return out


def io_delta(before, after):
    return {k: after[k] - before[k] for k in IO_FIELDS}


def png_inventory(root):
    total = 0
    count = 0
    for path in Path(root).rglob("*.png"):
        try:
            total += path.stat().st_size
            count += 1
        except OSError:
            continue
    return {"count": count, "bytes": total}


def png_delta(before, after):
    return {"count": after["count"] - before["count"],
            "bytes": after["bytes"] - before["bytes"]}


def ms(x):
    return round(x * 1000.0, 3)


class TaskRunner:
    def __init__(self, daemon, serve_pid, scratch, session_id, tag, index):
        self.daemon = daemon
        self.serve_pid = serve_pid
        self.scratch = Path(scratch)
        self.session_id = session_id
        self.tag = tag
        self.index = index
        self.base = {"request_id": "p5-%s-%d" % (tag, index),
                     "session_id": session_id}
        self.screens = self.scratch / "screens"
        self.screens.mkdir(parents=True, exist_ok=True)
        self.req = 0
        self.pids = descendants(self.serve_pid)

    def call(self, step, tool, args, pngs_before):
        self.req += 1
        io0 = io_snapshot(self.pids)
        t0 = time.time()
        wall0 = time.perf_counter()
        result = self.daemon.call(tool, args)
        wall = time.perf_counter() - wall0
        t1 = time.time()
        io1 = io_snapshot(self.pids)
        env = result.get("envelope") or {}
        value = env.get("result")
        record = {
            "step": step,
            "tool": tool,
            "ms": round(result["wall_ms"], 3),
            "t0_us": int(t0 * 1e6),
            "t1_us": int(t1 * 1e6),
            "ok": bool(env.get("ok")),
            "status": env.get("status"),
            "response_bytes": result.get("response_bytes"),
            "result_bytes": (len(json.dumps(value, separators=(",", ":")).encode())
                             if value is not None else 0),
            "io": io_delta(io0, io1),
            "png_new": png_delta(pngs_before, png_inventory(self.scratch)),
            "error": (env.get("error") or {}).get("code") if env.get("error") else None,
            "wall_s": wall,
        }
        if not record["ok"] and env.get("error"):
            record["error_message"] = (env.get("error") or {}).get("message")
        return record, value

    def task(self):
        rows = {}
        pngs = png_inventory(self.scratch)
        steps = [
            ("launch", "acos_launch", {"base": self.base, "apk": str(APK)}),
            ("inspect", "acos_observe_tree", {"base": self.base, "max_nodes": 2000}),
            ("tap", "acos_tap", {"base": self.base, "id": TAP_ID}),
            ("observe_tree", "acos_observe",
             {"base": self.base, "tree": True, "text": True}),
            ("observe_png", "acos_observe",
             {"base": self.base, "tree": False, "text": False, "png": True,
              "screenshot": str(self.screens / ("%s-%03d.png" % (self.tag, self.index)))}),
            ("assert", "acos_observe_tree", {"base": self.base, "max_nodes": 2000}),
            ("reset", "acos_reset", {"base": self.base}),
        ]
        self.pids = descendants(self.serve_pid)
        t_loop = time.time()
        for step, tool, args in steps:
            before = pngs
            record, value = self.call(step, tool, args, before)
            pngs = png_inventory(self.scratch)
            record["tree_nodes"] = count_nodes(tree_of(value))
            record["tree_bytes"] = (len(json.dumps(tree_of(value),
                                                   separators=(",", ":")).encode())
                                    if tree_of(value) is not None else 0)
            rows[step] = record
            rows[step]["_value"] = value
        loop_s = time.time() - t_loop

        # -- verification: never trust an acknowledgement alone ------------- #
        pre_tree = tree_of(rows["inspect"]["_value"])
        post_tree = tree_of(rows["assert"]["_value"])
        pre_text = node_text(find_node(pre_tree, ASSERT_ID))
        post_text = node_text(find_node(post_tree, ASSERT_ID))
        shot = rows["observe_png"]["_value"] or {}
        shot_meta = shot.get("screenshot") or {}
        shot_path = shot_meta.get("path")
        shot_bytes = os.path.getsize(shot_path) if shot_path and os.path.exists(shot_path) else 0
        reset_value = rows["reset"]["_value"] or {}
        checks = {
            "launch_ok": rows["launch"]["ok"] and count_nodes(tree_of(rows["launch"]["_value"])) > 0,
            "inspect_ok": rows["inspect"]["ok"] and pre_text == PRE_TEXT,
            "tap_ok": rows["tap"]["ok"],
            "observe_tree_ok": rows["observe_tree"]["ok"] and count_nodes(
                tree_of(rows["observe_tree"]["_value"])) > 0,
            "observe_png_ok": rows["observe_png"]["ok"] and shot_bytes > 0,
            "assert_ok": rows["assert"]["ok"] and post_text == POST_TEXT,
            "reset_ok": (rows["reset"]["ok"] and bool(reset_value.get("state_reset"))
                         and bool(reset_value.get("data_cleared"))
                         and (reset_value.get("frame_seq") or 0) >
                         ((rows["tap"]["_value"] or {}).get("last_frame_seq") or 0)),
        }
        verdict = "pass" if all(checks.values()) else "fail"
        for step in rows:
            rows[step].pop("_value", None)
            rows[step]["ms_step_total"] = rows[step]["ms"]
        return {
            "index": self.index,
            "tag": self.tag,
            "session_id": self.session_id,
            "utc": benchlib.utc_now(),
            "loop_ms": round(sum(r["ms"] for r in rows.values()), 3),
            "loop_wall_ms": ms(loop_s),
            "verdict": verdict,
            "checks": checks,
            "pre_text": pre_text,
            "post_text": post_text,
            "screenshot_bytes": shot_bytes,
            "steps": rows,
        }


def run_agent(args, daemon, serve_pid, scratch, agent_index, results, barrier=None):
    tag = "n%d-a%d" % (args.n, agent_index)
    session_id = "p5_%s" % tag
    runner = TaskRunner(daemon, serve_pid, scratch, session_id, tag, 0)
    base = {"request_id": "p5-launch-%s" % tag, "session_id": session_id}
    launch, value = runner.call("launch0", "acos_launch",
                               {"base": base, "apk": str(APK)},
                               png_inventory(scratch))
    rows = [{"agent": agent_index, "warmup": True, "bootstrap": launch,
             "verdict": "warmup"}]
    if barrier is not None:
        barrier.wait()
    for i in range(args.loops):
        runner.index = agent_index * 100000 + i
        row = runner.task()
        row["agent"] = agent_index
        row["warmup"] = i < args.warmup
        row["measured_verdict"] = row["verdict"]
        if row["warmup"]:
            row["verdict"] = "warmup"
        rows.append(row)
    results[agent_index] = rows


def parse_traces(scratch, tasks):
    """Attach the existing env-gated trace events to each request by time window."""
    events = []
    seen = set()
    for name, prefix in (("p3.trace", "P3TRACE"), ("dm5.trace", "DM5TRACE")):
        path = Path(scratch) / name
        if not path.exists():
            continue
        for line in path.read_text(errors="replace").splitlines():
            parts = line.split(" ", 1)
            if len(parts) != 2 or parts[0] not in ("P3TRACE", "DM5TRACE"):
                continue
            fields = {}
            for token in parts[1].split():
                if "=" in token:
                    k, _, v = token.partition("=")
                    fields[k] = v
            try:
                t_us = int(fields.get("t_us", 0))
                dur = int(fields.get("dur_us", 0))
            except ValueError:
                continue
            key = (fields.get("proc"), fields.get("ev"), t_us, dur,
                   " ".join(t for t in parts[1].split() if "=" not in t))
            # P3 and DM5 both trace some phases with the same clock; keep one.
            if key in seen:
                continue
            seen.add(key)
            events.append({"proc": fields.get("proc"), "ev": fields.get("ev"),
                           "t_us": t_us, "dur_us": dur, "raw": line})
    dm6 = []
    path = Path(scratch) / "dm6.trace"
    if path.exists():
        for line in path.read_text(errors="replace").splitlines():
            try:
                dm6.append(json.loads(line))
            except ValueError:
                pass
    for task in tasks:
        for step in task.get("steps", {}).values():
            t0, t1 = step["t0_us"], step["t1_us"]
            step["trace"] = [e for e in events if t0 <= e["t_us"] <= t1]
    return len(events), dm6


def stats_rows(tasks, key="steps"):
    per = {}
    for task in tasks:
        if task.get("verdict") == "warmup":
            continue
        for step, record in task.get(key, {}).items():
            per.setdefault(step, []).append(record["ms"])
    return {step: benchlib.stats(vals) for step, vals in per.items()}


def main(argv=None):
    p = argparse.ArgumentParser()
    p.add_argument("--acos", required=True)
    p.add_argument("--out", required=True)
    p.add_argument("--scratch", required=True)
    p.add_argument("--n", type=int, default=1)
    p.add_argument("--loops", type=int, default=22)
    p.add_argument("--warmup", type=int, default=1)
    p.add_argument("--settle", type=float, default=0.0,
                   help="idle seconds before the measured loops (default 0)")
    args = p.parse_args(argv)

    scratch = Path(args.scratch)
    scratch.mkdir(parents=True, exist_ok=True)
    os.environ["P3_TRACE"] = str(scratch / "p3.trace")
    os.environ["DM5_TRACE"] = str(scratch / "dm5.trace")
    os.environ["DM6_RESET_TRACE"] = str(scratch / "dm6.trace")

    provenance = benchlib.binary_provenance(args.acos)
    host = benchlib.read_meminfo()
    host.update(benchlib.read_loadavg())
    serve = None
    sampler = None
    doc = {
        "schema": "acos-p5-loop/1",
        "utc": benchlib.utc_now(),
        "n": args.n,
        "loops_per_agent": args.loops,
        "warmup": args.warmup,
        "apk": str(APK),
        "apk_sha256": benchlib.sha256_file(APK),
        "binary": args.acos,
        "binary_sha256": provenance.get("sha256"),
        "profile": provenance.get("profile"),
        "verdict_eligible_binary": provenance.get("verdict_eligible"),
        "host_before": host,
        "tasks": [],
        "ok": False,
    }
    t_start = None
    try:
        serve = Serve(args.acos, scratch, max_sessions=max(8, args.n))
        job = benchlib.cgroup_path(serve.proc.pid)
        doc["job_cgroup"] = str(job)
        sampler = WarmSampler(job, os.getpid(), interval=0.1)
        sampler.start()
        clients = [serve.client() for _ in range(args.n)]
        if args.settle:
            time.sleep(args.settle)
        results = {}
        threads = []
        barrier = threading.Barrier(args.n) if args.n > 1 else None
        t_start = time.time()
        for i in range(args.n):
            th = threading.Thread(target=run_agent,
                                  args=(args, clients[i], serve.proc.pid, scratch,
                                        i, results, barrier))
            th.start()
            threads.append(th)
        for th in threads:
            th.join()
        t_end = time.time()
    finally:
        if sampler is not None:
            sampler.stop()
        if serve is not None:
            doc["serve_rc"] = serve.close()
    samples = sampler.samples if sampler else []
    doc["sampler_errors"] = sampler.errors if sampler else []

    rows = [row for agent in sorted(results) for row in results[agent]]
    doc["tasks"] = rows
    doc["loop"] = stats_rows(rows)
    doc["loop_total"] = benchlib.stats([r["loop_ms"] for r in rows
                                        if r.get("verdict") != "warmup"])
    doc["loop_wall_total"] = benchlib.stats([r["loop_wall_ms"] for r in rows
                                             if r.get("verdict") != "warmup"])
    doc["trace_events"] = parse_traces(scratch, rows)[0]
    doc["dm6_events"] = parse_traces(scratch, rows)[1]
    passes = sum(1 for r in rows if r.get("verdict") == "pass")
    measured = sum(1 for r in rows if r.get("verdict") != "warmup")
    doc["verified"] = passes
    doc["measured_tasks"] = measured
    doc["false_pass"] = sum(1 for r in rows if r.get("verdict") == "pass"
                            and not all(r.get("checks", {}).values()))
    doc["failed_tasks"] = [{"agent": r.get("agent"), "index": r.get("index"),
                            "checks": r.get("checks")}
                           for r in rows if r.get("verdict") == "fail"]
    if samples:
        t_from, t_to = t_start, t_end
        win = [s for s in samples if t_from <= s["t"] <= t_to]
        cur = [{"t": s["t"], "bytes": (s.get("job_memory_current_mib") or 0) * 1048576}
               for s in win]
        net = [{"t": s["t"], "bytes": (s.get("net_anon_shmem_mib") or 0) * 1048576}
               for s in win]
        peak = max((s.get("job_memory_current_mib") or 0) for s in win) if win else None
        doc["memory"] = {
            "samples": len(win),
            "interval_s": 0.1,
            "cgroup_memory_current_peak_mib": round(peak, 3) if peak else None,
            "cgroup_gib_min": benchlib.sample_integral(cur, "bytes"),
            "net_anon_shmem_gib_min": benchlib.sample_integral(net, "bytes"),
            "net_anon_shmem_peak_mib": (max(s["bytes"] for s in net) / 1048576.0
                                        if net else None),
            "last": win[-1] if win else None,
        }
        cohort_s = (t_end - t_start) if t_start else None
        doc["throughput"] = {
            "cohort_wall_s": round(cohort_s, 3) if cohort_s else None,
            "verified_tasks": passes,
            "verified_tasks_per_s": round(passes / cohort_s, 4) if cohort_s else None,
            "verified_tasks_per_gib_min": (round(passes / doc["memory"]["cgroup_gib_min"], 4)
                                           if doc["memory"]["cgroup_gib_min"] else None),
            "verified_tasks_per_gib_min_net": (
                round(passes / doc["memory"]["net_anon_shmem_gib_min"], 4)
                if doc["memory"]["net_anon_shmem_gib_min"] else None),
        }
    doc["host_after"] = benchlib.read_meminfo()
    doc["host_after"].update(benchlib.read_loadavg())
    doc["ok"] = passes == measured and measured > 0 and doc["false_pass"] == 0
    Path(args.out).parent.mkdir(parents=True, exist_ok=True)
    Path(args.out).write_text(json.dumps(doc, indent=1))
    print("p5: n=%d measured=%d pass=%d false_pass=%d ok=%s -> %s"
          % (args.n, measured, passes, doc["false_pass"], doc["ok"], args.out))
    return 0 if doc["ok"] else 1


if __name__ == "__main__":
    sys.exit(main())
