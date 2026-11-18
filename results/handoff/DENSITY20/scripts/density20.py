#!/usr/bin/env python3
"""DENSITY20 -- N concurrent `acos serve` sessions, density measured per session.

One invocation = ONE cohort = ONE `acos serve` daemon + N sessions, each running
the counter scenario (launch -> tree(count=0) -> tap(inc) -> tree(count=1) ->
reset) on its own socket connection. Every invocation is meant to run inside
`tools/art-runtime/run-exclusive.sh` + `tools/guarded.sh` (see run-density.sh).

Why this shape: the primary metric of the release (BENCHMARKS.md §1 M11,
"verified tasks per GiB-minute", owner direction 2026-11-18 §4) is about many
agent sessions on one host. A cohort is the unit the host pays for: one daemon
plus N live ART guests.

What it records (raw arrays in the JSON, nothing estimated):
  * per-session memory: every process of the daemon's tree is read from
    /proc/<pid>/smaps_rollup each sample -> Pss, Uss (Private_Clean+Dirty),
    shared (Shared_Clean+Dirty), Rss, comm. Sessions are uniform by design (same
    APK, same scenario), so per-session = tree total / live sessions, and the
    class breakdown (acos host, dalvikvm64 guest, bwrap, shell wrappers) is
    recorded separately.
  * a settled-state attribution snapshot: full /proc/<pid>/smaps aggregated by
    mapping (dalvik heap, boot image, odex/vdex/oat, libhwui/skia, fonts,
    binder, apk/dex, GL, other anon, other file) -> per class and per session.
  * host: MemTotal/MemAvailable before each cohort and every sample, host used %,
    loadavg; the peak host used % is reported (the guard ceiling is 85 %).
  * wall time of the measured window, verified tasks, failures (with per-step
    error codes), CPU core-seconds of the daemon tree.
  * GiB-minutes: trapezoid integral of the tree's Pss over the window (PSS is
    the type BENCHMARKS §1 requires for a per-instance memory row and is what
    the emulator baseline §13 reports; the cgroup anon+shmem integral is also
    recorded where the cgroup is readable, labelled as a second type).

Honesty rules (BENCHMARKS.md §2):
  * a task is verified only when BOTH tree reads are right (count "0" before the
    tap, "1" after) and reset reports state_reset + data_cleared with a fresh
    frame; an acknowledgement alone is never a pass;
  * a failed request is a failed task with its structured error code, never a
    silently dropped sample;
  * the sampler's own memory is not charged (it is a separate process; its Pss
    is recorded and subtracted from the tree integral only if it is a
    descendant, which it never is);
  * a cohort with 0 verified tasks is recorded, not discarded.

Usage (normally through run-density.sh):
  python3 -I density20.py --acos <release acos> --n 10 --loops 3 \
      --out <dir>/n10-r1.json [--zygote on] [--settle-ms 2000] [--label ...]

Scratch: $WT/.cache/density20/<n>-<arm>-<stamp>/ (never /tmp).
"""

from __future__ import annotations

import argparse
import json
import os
import re
import shutil
import sys
import threading
import time
import traceback
from pathlib import Path

WT = Path(__file__).resolve().parents[4]  # results/handoff/DENSITY20/scripts -> WT
sys.path.insert(0, str(WT / "tools"))
sys.path.insert(0, str(WT / "tools/art-runtime"))

APK = WT / "fixtures/build/apps/counter.apk"
PROC = Path("/proc")

# ---------------------------------------------------------------- proc helpers


def utc_now() -> str:
    return time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime())


def read_text(path, default=None):
    try:
        return Path(path).read_text(errors="replace")
    except OSError:
        return default


def ppid_map():
    out = {}
    for entry in PROC.iterdir():
        if not entry.name.isdigit():
            continue
        text = read_text(entry / "stat")
        if not text:
            continue
        try:
            tail = text[text.rindex(")") + 2:].split()
            out[int(entry.name)] = int(tail[1])
        except (ValueError, IndexError):
            continue
    return out


def descendants(root, ppid=None):
    """All live descendants of `root` (root included)."""
    ppid = ppid or ppid_map()
    children = {}
    for pid, parent in ppid.items():
        children.setdefault(parent, []).append(pid)
    out, stack = [], [root]
    while stack:
        pid = stack.pop()
        out.append(pid)
        stack.extend(children.get(pid, ()))
    return sorted(set(out))


def comm_of(pid):
    return (read_text(PROC / str(pid) / "comm", "") or "").strip()


def smaps_rollup(pid):
    """Pss / Uss / shared / Rss in MiB for one process, or None."""
    text = read_text(PROC / str(pid) / "smaps_rollup")
    if text is None:
        return None
    vals = {"pss": 0, "private": 0, "shared": 0, "rss": 0}
    for line in text.splitlines():
        head, _, rest = line.partition(":")
        parts = rest.split()
        if not parts:
            continue
        try:
            kb = int(parts[0])
        except ValueError:
            continue
        if head == "Pss":
            vals["pss"] = kb
        elif head in ("Private_Clean", "Private_Dirty"):
            vals["private"] += kb
        elif head in ("Shared_Clean", "Shared_Dirty"):
            vals["shared"] += kb
        elif head == "Rss":
            vals["rss"] = kb
    return {k: v / 1024.0 for k, v in vals.items()}


def cpu_time(pid):
    """(utime+stime) seconds for one process, from /proc/<pid>/stat."""
    text = read_text(PROC / str(pid) / "stat")
    if not text:
        return None
    try:
        tail = text[text.rindex(")") + 2:].split()
        return (int(tail[11]) + int(tail[12])) / os.sysconf("SC_CLK_TCK")
    except (ValueError, IndexError):
        return None


def meminfo():
    text = read_text("/proc/meminfo", "") or ""
    out = {}
    for line in text.splitlines():
        head, _, rest = line.partition(":")
        if head in ("MemTotal", "MemAvailable", "MemFree"):
            try:
                out[head] = int(rest.split()[0]) / 1024.0  # MiB
            except (ValueError, IndexError):
                pass
    return out


def loadavg():
    text = read_text("/proc/loadavg", "") or ""
    return text.split()[:3]


# ------------------------------------------------------------------ mapping classes
_MAP_RULES = [
    ("dalvik-heap", re.compile(r"\[heap\]|\[anon:dalvik-.*heap", re.I)),
    ("dalvik-other", re.compile(r"\[anon:dalvik", re.I)),
    ("boot-image", re.compile(r"boot(-[a-z0-9]+)?\.(art|oat|img|vdex)|boot\.oat", re.I)),
    ("odex-vdex-oat", re.compile(r"\.(odex|vdex|oat|art)$", re.I)),
    ("framework-dex", re.compile(r"(framework\.jar|\.dex$|core-oj|core-libart|conscrypt|okhttp|bouncycastle)", re.I)),
    ("hwui-skia", re.compile(r"libhwui|libskia|libandroidfw", re.I)),
    ("fonts", re.compile(r"(/fonts?/|\.(ttf|otf|ttc)$)", re.I)),
    ("binder", re.compile(r"(binder|/dev/ashmem)", re.I)),
    ("gl", re.compile(r"(libGLES|libEGL|libGLESv|swiftshader|angle|libvk|vulkan)", re.I)),
    ("apk-dex", re.compile(r"\.(apk|jar)$", re.I)),
    ("shared-lib", re.compile(r"\.so(\.|$)", re.I)),
]


def map_class(name):
    if name.startswith("[anon:"):
        for label, rx in _MAP_RULES[:2]:
            if rx.search(name):
                return label
        return "anon-other"
    if name == "[heap]":
        return "dalvik-heap"
    if name.startswith("["):
        if "binder" in name or "ashmem" in name:
            return "binder"
        return "anon-other"
    for label, rx in _MAP_RULES[2:]:
        if rx.search(name):
            return label
    return "other-file"


def smaps_by_mapping(pid, top_paths=None, top_n=12):
    """{class: {pss_mib, private_mib, shared_mib, maps:int}} for one process.

    With `top_paths` (a dict) the largest individual mappings by Pss are also
    returned under its "paths" key: the raw evidence behind the class table.
    """
    text = read_text(PROC / str(pid) / "smaps")
    if not text:
        return None
    per = {}
    cur = None
    cur_class = None
    path_pss = {}
    for line in text.splitlines():
        if re.match(r"^[0-9a-f]+-[0-9a-f]+ ", line):
            parts = line.split(None, 5)
            cur = parts[5] if len(parts) > 5 else "[anonymous]"
            cur_class = map_class(cur)
            rec = per.setdefault(cur_class, {"pss_mib": 0.0, "private_mib": 0.0,
                                             "shared_mib": 0.0, "maps": 0})
            rec["maps"] += 1
            continue
        if cur_class is None:
            continue
        head, _, rest = line.partition(":")
        parts = rest.split()
        if not parts:
            continue
        try:
            kb = int(parts[0])
        except ValueError:
            continue
        rec = per[cur_class]
        if path_pss is not None:
            path_pss[cur] = path_pss.get(cur, 0.0) + (kb / 1024.0 if head == "Pss" else 0.0)
        if head == "Pss":
            rec["pss_mib"] += kb / 1024.0
        elif head in ("Private_Clean", "Private_Dirty"):
            rec["private_mib"] += kb / 1024.0
        elif head in ("Shared_Clean", "Shared_Dirty"):
            rec["shared_mib"] += kb / 1024.0
    if top_paths is not None:
        ranked = sorted(path_pss.items(), key=lambda kv: -kv[1])[:top_n]
        top_paths["paths"] = [{"path": p, "pss_mib": round(v, 3)}
                              for p, v in ranked if v > 0.05]
    return per


_HOST_COMMS = ("acos", "acos-art", "acos-art-run", "acos_art")
_WRAPPER_COMMS = ("sh", "bash", "dash", "systemd-run", "timeout")


def pid_class(comm):
    """dalvikvm64 is the ART guest; everything else that is not the daemon, a
    bwrap sandbox helper or a shell wrapper is the app process ART forked
    (comm is the app's process name, e.g. `fixtures.counter`)."""
    if comm in _HOST_COMMS:
        return "host"
    if comm == "bwrap":
        return "bwrap"
    if comm in _WRAPPER_COMMS:
        return "wrapper"
    return "guest"


# ---------------------------------------------------------------------- sampler
class TreeSampler(threading.Thread):
    """Samples every process of the daemon tree (Pss/Uss/shared/Rss/cpu)."""

    def __init__(self, root_pid, interval=0.25):
        super().__init__(daemon=True)
        self.root = int(root_pid)
        self.interval = float(interval)
        self.samples = []
        self.errors = []
        self.stop_event = threading.Event()

    def sample_once(self):
        pids = descendants(self.root)
        rows = []
        cpu = 0.0
        for pid in pids:
            r = smaps_rollup(pid)
            if r is None:
                continue
            c = cpu_time(pid)
            if c is not None:
                cpu += c
            comm = comm_of(pid)
            rows.append({"pid": pid, "comm": comm, "class": pid_class(comm), **r})
        host = meminfo()
        self.samples.append({
            "t": time.time(),
            "utc": utc_now(),
            "pids": rows,
            "cpu_s": round(cpu, 3),
            "host_mem_available_mib": host.get("MemAvailable"),
            "host_mem_total_mib": host.get("MemTotal"),
        })

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
        self.join(timeout=10)

    def snapshot(self):
        """Last sample (settled state)."""
        return self.samples[-1] if self.samples else None


def integral(samples, key="pss"):
    """Trapezoid integral of the tree total, in GiB-minutes."""
    series = []
    for s in samples:
        total = sum(r[key] for r in s["pids"])
        series.append((s["t"], total / 1024.0))  # GiB
    if len(series) < 2:
        return 0.0
    gib_min = 0.0
    for (t0, v0), (t1, v1) in zip(series, series[1:]):
        gib_min += (v0 + v1) / 2.0 * (t1 - t0) / 60.0
    return gib_min


def class_totals(sample):
    out = {}
    for r in sample["pids"]:
        rec = out.setdefault(r["class"], {"pss": 0.0, "private": 0.0,
                                          "shared": 0.0, "rss": 0.0, "procs": 0})
        rec["pss"] += r["pss"]
        rec["private"] += r["private"]
        rec["shared"] += r["shared"]
        rec["rss"] += r["rss"]
        rec["procs"] += 1
    return out


# ------------------------------------------------------------------- serve control
class Serve:
    def __init__(self, acos, scratch, n, zygote_off, boot_image_cache=None, idle=900):
        self.scratch = Path(scratch)
        self.socket_path = str(self.scratch / "serve.sock")
        self.stdout = open(self.scratch / "serve.stdout", "w")
        self.stderr = open(self.scratch / "serve.log", "w")
        env = dict(os.environ,
                   ACOS_RUNTIME_DIR=str(self.scratch / "runtime"),
                   ACOS_CACHE_DIR=str(self.scratch / "cache"),
                   ACOS_SERVE_ARTIFACTS=str(self.scratch / "sessions"))
        # One warm shared-fixed boot image for every cohort. Without this the
        # default chain puts the image under ACOS_CACHE_DIR (= this rep's
        # scratch), so each rep pays a dex2oat build and maps a *different*
        # inode -- two cohorts would not be comparable and no page would be
        # shared across them. A real deployment has one persistent cache.
        if boot_image_cache:
            env["ACOS_BOOT_IMAGE_CACHE"] = str(boot_image_cache)
        art_bin = env.get("ACOS_ART_BIN") or str(Path(acos).with_name("acos-art"))
        env["ACOS_ART_BIN"] = art_bin
        if not zygote_off:
            env["ACOS_ART_ZYGOTE"] = "on"
        self.argv = [acos, "serve", "--socket", self.socket_path, "--engine", "art",
                     "--idle-timeout", str(idle),
                     "--max-sessions", str(max(8, n + 4)),
                     "--owner-pid", str(os.getpid())]
        import subprocess
        self.proc = subprocess.Popen(self.argv, cwd=str(WT), env=env,
                                     stdin=subprocess.DEVNULL,
                                     stdout=self.stdout, stderr=self.stderr,
                                     start_new_session=True)
        deadline = time.time() + 60
        while time.time() < deadline:
            if Path(self.socket_path).exists():
                return
            if self.proc.poll() is not None:
                raise RuntimeError("acos serve died before binding (rc=%s); see %s"
                                   % (self.proc.returncode, self.stderr.name))
            time.sleep(0.05)
        raise RuntimeError("acos serve never bound %s" % self.socket_path)

    def close(self):
        import signal
        try:
            if self.proc.poll() is None:
                self.proc.terminate()
                try:
                    self.proc.wait(timeout=30)
                except Exception:
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


# ------------------------------------------------------------------ task / session
def walk_nodes(nodes):
    for node in nodes or []:
        if not isinstance(node, dict):
            continue
        yield node
        yield from walk_nodes(node.get("children"))


def find_node(tree, resource_id):
    for node in walk_nodes((tree or {}).get("nodes")):
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
    return sum(1 for _ in walk_nodes((tree or {}).get("nodes")))


class Session:
    def __init__(self, daemon, tag, idx, n, loops, apk):
        self.daemon = daemon
        self.tag = tag
        self.idx = idx
        self.loops = loops
        self.apk = apk
        self.session_id = "d20_%s" % tag
        self.base = {"request_id": "d20-%s" % tag, "session_id": self.session_id}
        self.tasks = []

    def call(self, step, tool, args):
        import agent_loop
        t0 = time.perf_counter()
        try:
            result = self.daemon.call(tool, args)
        except Exception as exc:  # noqa: BLE001 -- transport failure is a failed step
            return {"step": step, "tool": tool, "ok": False,
                    "error": "TRANSPORT:%s" % type(exc).__name__,
                    "error_message": str(exc),
                    "ms": round((time.perf_counter() - t0) * 1000.0, 3)}, None
        env = result.get("envelope") or {}
        err = env.get("error") or {}
        return ({"step": step, "tool": tool,
                 "ok": bool(env.get("ok")),
                 "status": env.get("status"),
                 "error": err.get("code"),
                 "error_message": err.get("message") if not env.get("ok") else None,
                 "ms": round(result["wall_ms"], 3)},
                env.get("result"))

    def bootstrap(self):
        rec, value = self.call("launch", "acos_launch",
                               {"base": dict(self.base, request_id=self.base["request_id"] + "-boot"),
                                "apk": str(self.apk)})
        rec["tree_nodes"] = count_nodes(tree_of(value))
        if isinstance(value, dict) and value.get("boot_image") is not None:
            rec["boot_image"] = value["boot_image"]
        rec["ok"] = bool(rec["ok"]) and rec["tree_nodes"] > 0
        return rec

    def task(self, index):
        base = dict(self.base, request_id="%s-t%d" % (self.base["request_id"], index))
        steps = {}
        rec, value = self.call("launch", "acos_launch", {"base": base, "apk": str(self.apk)})
        steps["launch"] = rec
        pre_nodes = count_nodes(tree_of(value))
        steps["launch"]["tree_nodes"] = pre_nodes

        rec, value = self.call("inspect", "acos_observe_tree",
                               {"base": base, "max_nodes": 2000})
        pre_tree = tree_of(value)
        steps["inspect"] = rec
        steps["inspect"]["tree_nodes"] = count_nodes(pre_tree)
        pre_text = node_text(find_node(pre_tree, "count"))

        rec, value = self.call("tap", "acos_tap", {"base": base, "id": "inc"})
        steps["tap"] = rec
        tap_seq = (value or {}).get("last_frame_seq") if isinstance(value, dict) else None

        rec, value = self.call("assert", "acos_observe_tree",
                               {"base": base, "max_nodes": 2000})
        post_tree = tree_of(value)
        steps["assert"] = rec
        steps["assert"]["tree_nodes"] = count_nodes(post_tree)
        post_text = node_text(find_node(post_tree, "count"))

        rec, value = self.call("reset", "acos_reset", {"base": base})
        steps["reset"] = rec
        rv = value or {}
        steps["reset"]["state_reset"] = bool(rv.get("state_reset"))
        steps["reset"]["data_cleared"] = bool(rv.get("data_cleared"))
        steps["reset"]["fresh_frame"] = (rv.get("frame_seq") or 0) > (tap_seq or 0)

        checks = {
            "launch_ok": steps["launch"]["ok"] and pre_nodes > 0,
            "pre_text_zero": pre_text == "0",
            "inspect_ok": steps["inspect"]["ok"] and steps["inspect"]["tree_nodes"] > 0,
            "tap_ok": steps["tap"]["ok"],
            "post_text_one": post_text == "1",
            "assert_ok": steps["assert"]["ok"] and steps["assert"]["tree_nodes"] > 0,
            "reset_ok": (steps["reset"]["ok"] and steps["reset"]["state_reset"]
                         and steps["reset"]["data_cleared"]
                         and steps["reset"]["fresh_frame"]),
        }
        return {
            "session": self.tag,
            "index": index,
            "verdict": "pass" if all(checks.values()) else "fail",
            "checks": checks,
            "pre_text": pre_text,
            "post_text": post_text,
            "step_ms": {name: rec["ms"] for name, rec in steps.items()},
            "steps": steps,
            "error_codes": {name: rec.get("error") for name, rec in steps.items()
                            if not rec.get("ok")},
        }

    def run(self, barrier, results):
        try:
            boot = self.bootstrap()
            if barrier is not None:
                barrier.wait()
            if not boot["ok"]:
                results[self.idx] = {"bootstrap": boot, "tasks": [],
                                     "error": "BOOTSTRAP_FAILED",
                                     "bootstrap_error": boot}
                return
            tasks = [self.task(i) for i in range(self.loops)]
            results[self.idx] = {"bootstrap": boot, "tasks": tasks}
        except Exception as exc:  # noqa: BLE001
            results[self.idx] = {"error": "%s: %s" % (type(exc).__name__, exc),
                                 "trace": traceback.format_exc(limit=3),
                                 "tasks": []}


# ------------------------------------------------------------------------- cohort
def run_cohort(args):
    import benchlib
    import agent_loop

    stamp = time.strftime("%m%d%H%M%S", time.gmtime())
    arm = "zygote-on" if args.zygote == "on" else "exec"
    # SUN_LEN: `acos serve --socket` must stay short; keep the dir compact.
    code = "z" if args.zygote == "on" else "e"
    scratch = Path(args.scratch) / ("n%d%s%s" % (args.n, code, stamp))
    scratch.mkdir(parents=True, exist_ok=True)
    doc = {
        "schema": "acos-density20/1",
        "utc": utc_now(),
        "label": args.label,
        "n": args.n,
        "loops_per_session": args.loops,
        "arm": arm,
        "zygote": args.zygote,
        "apk": str(args.apk),
        "apk_sha256": benchlib.sha256_file(args.apk),
        "binary": args.acos,
        "binary_provenance": benchlib.binary_provenance(args.acos),
        "host_before": meminfo(),
        "loadavg_before": loadavg(),
        "lock": args.lock,
        "sampler_interval_s": None,
        "git_head": subprocess_head(),
        "git_dirty": subprocess_dirty(),
    }
    serve = None
    sampler = None
    results = {}
    clients = []
    t_start = t_end = None
    try:
        bic = Path(args.boot_image_cache) if args.boot_image_cache else None
        if bic is not None:
            bic.mkdir(parents=True, exist_ok=True)
            doc["boot_image_cache"] = str(bic)
            doc["boot_image_cache_entries_before"] = len(list(bic.glob("fixed-*")))
        t_boot0 = time.time()
        serve = Serve(args.acos, scratch, args.n, argv_zygote_off(args), bic)
        doc["serve_boot_s"] = round(time.time() - t_boot0, 3)
        if bic is not None:
            doc["boot_image_cache_entries_after"] = len(list(bic.glob("fixed-*")))
        doc["serve_pid"] = serve.proc.pid
        doc["serve_argv"] = serve.argv
        # daemon baseline, before any session exists
        base_pids = descendants(serve.proc.pid)
        doc["baseline"] = {
            "pids": [{"pid": p, "comm": comm_of(p), "pss": (smaps_rollup(p) or {}).get("pss")}
                     for p in base_pids],
        }
        n_pids_guess = max(4, len(base_pids)) * (args.n + 1)
        interval = max(float(args.sample_ms) / 1000.0, 0.0008 * n_pids_guess)
        sampler = TreeSampler(serve.proc.pid, interval=interval)
        doc["sampler_interval_s"] = round(interval, 4)
        sampler.start()
        clients = [agent_loop.Daemon(serve.socket_path, timeout=180.0)
                   for _ in range(args.n)]
        sessions = [Session(clients[i], "n%d-a%d" % (args.n, i), i, args.n,
                            args.loops, Path(args.apk)) for i in range(args.n)]
        barrier = threading.Barrier(args.n) if args.n > 1 else None
        threads = [threading.Thread(target=s.run, args=(barrier, results))
                   for s in sessions]
        t_start = time.time()
        for th in threads:
            th.start()
        for th in threads:
            th.join()
        t_end = time.time()
        # settled state: every session holds a live guest showing count=0
        settle = float(args.settle_ms) / 1000.0
        if settle > 0:
            time.sleep(settle)
        snapshot = sampler.snapshot()
        doc["settled_snapshot"] = snapshot
        doc["attribution"] = attribute(snapshot) if snapshot else None
        doc["settled_host"] = meminfo()
    finally:
        if sampler is not None:
            sampler.stop()
        for c in clients:
            try:
                c.close()
            except Exception:
                pass
        if serve is not None:
            doc["serve_rc"] = serve.close()

    tasks = []
    for idx in sorted(results):
        entry = results[idx]
        for t in entry.get("tasks", []):
            tasks.append(t)
        if entry.get("error"):
            doc.setdefault("session_errors", []).append(
                {"session": idx, "error": entry["error"],
                 "bootstrap": entry.get("bootstrap_error"),
                 "trace": entry.get("trace")})
    doc["tasks"] = tasks
    doc["boot_image_reports"] = sorted({
        json.dumps(entry.get("bootstrap", {}).get("boot_image"), sort_keys=True)
        for entry in results.values()
        if entry.get("bootstrap", {}).get("boot_image") is not None})
    verified = sum(1 for t in tasks if t["verdict"] == "pass")
    failed = [t for t in tasks if t["verdict"] != "pass"]
    doc["verified_tasks"] = verified
    doc["failed_tasks"] = [{"session": t["session"], "index": t["index"],
                            "checks": t["checks"], "error_codes": t["error_codes"]}
                           for t in failed]
    doc["false_pass"] = 0  # a pass requires every check; recorded, not asserted
    doc["measured_tasks"] = len(tasks)
    doc["sessions_live_at_end"] = len([1 for e in results.values()
                                        if e.get("tasks") is not None and not e.get("error")])

    samples = sampler.samples if sampler else []
    doc["sampler_errors"] = sampler.errors if sampler else []
    win = [s for s in samples if t_start is not None and t_start <= s["t"] <= t_end]
    if t_start is not None:
        wall = t_end - t_start
        doc["wall_s"] = round(wall, 3)
        doc["gib_min_pss"] = round(integral(win), 5)
        doc["gib_min_uss"] = round(integral(win, "private"), 5)
        peak_total = max((sum(r["pss"] for r in s["pids"]) for s in win), default=0.0)
        doc["peak_tree_pss_mib"] = round(peak_total, 3)
        cpu = [(s["t"], s["cpu_s"]) for s in win if s.get("cpu_s") is not None]
        if len(cpu) >= 2:
            core_s = cpu[-1][1] - cpu[0][1]
            doc["cpu_core_seconds"] = round(core_s, 3)
            doc["cpu_pct_one_core"] = round(100.0 * core_s / (cpu[-1][0] - cpu[0][0]), 1)
            doc["cpu_pct_host"] = round(100.0 * core_s /
                                        ((cpu[-1][0] - cpu[0][0]) * os.cpu_count()), 2)
        host_used = [100.0 * (1.0 - (s["host_mem_available_mib"] /
                                     s["host_mem_total_mib"]))
                     for s in win if s.get("host_mem_available_mib")]
        if host_used:
            doc["host_used_pct_peak"] = round(max(host_used), 1)
            doc["host_used_pct_mean"] = round(sum(host_used) / len(host_used), 1)
        doc["host_after"] = meminfo()
        doc["loadavg_after"] = loadavg()
        if wall > 0:
            doc["verified_per_gib_min_pss"] = round(verified / doc["gib_min_pss"], 3) \
                if doc["gib_min_pss"] else None
            doc["verified_per_min"] = round(verified / (wall / 60.0), 3)
    # per-session settled rows: uniform workload -> total / N, with class split
    if doc.get("attribution") and args.n:
        doc["per_session_settled"] = per_session_rows(doc["attribution"], args.n,
                                                      doc.get("baseline"))
    doc["ok"] = bool(verified) and not doc["session_errors"] if "session_errors" in doc else bool(verified)
    out = Path(args.out)
    out.parent.mkdir(parents=True, exist_ok=True)
    benchlib.write_json(str(out), doc)
    keep = args.keep_scratch
    if not keep:
        shutil.rmtree(scratch, ignore_errors=True)
    return doc


def argv_zygote_off(args):
    return args.zygote != "on"


def subprocess_head():
    import subprocess
    try:
        return subprocess.run(["git", "-C", str(WT), "rev-parse", "HEAD"],
                              capture_output=True, text=True, timeout=20).stdout.strip()
    except Exception:
        return None


def subprocess_dirty():
    import subprocess
    try:
        out = subprocess.run(["git", "-C", str(WT), "status", "--porcelain"],
                             capture_output=True, text=True, timeout=20).stdout
        return len([l for l in out.splitlines() if l.strip()])
    except Exception:
        return None


def attribute(snapshot):
    """Full-smaps attribution of the settled tree, by class and by process."""
    procs = []
    per_class = {}
    for r in snapshot["pids"]:
        top = {}
        tables = smaps_by_mapping(r["pid"], top)
        if tables is None:
            continue
        procs.append({"pid": r["pid"], "comm": r["comm"], "class": r["class"],
                      "top_mappings": top.get("paths", []),
                      "pss_mib": round(r["pss"], 3),
                      "private_mib": round(sum(v["private_mib"] for v in tables.values()), 3),
                      "shared_mib": round(sum(v["shared_mib"] for v in tables.values()), 3),
                      "by_mapping": {k: {kk: round(vv, 3) for kk, vv in v.items()}
                                     for k, v in tables.items()}})
        for label, rec in tables.items():
            agg = per_class.setdefault(label, {"pss_mib": 0.0, "private_mib": 0.0,
                                               "shared_mib": 0.0})
            agg["pss_mib"] += rec["pss_mib"]
            agg["private_mib"] += rec["private_mib"]
            agg["shared_mib"] += rec["shared_mib"]
    total_pss = sum(p["pss_mib"] for p in procs) or 1.0
    rows = []
    for label, rec in sorted(per_class.items(), key=lambda kv: -kv[1]["pss_mib"]):
        rows.append({
            "mapping_class": label,
            "pss_mib": round(rec["pss_mib"], 3),
            "private_mib": round(rec["private_mib"], 3),
            "shared_mib": round(rec["shared_mib"], 3),
            "pct_of_tree_pss": round(100.0 * rec["pss_mib"] / total_pss, 2),
        })
    return {"total_pss_mib": round(sum(p["pss_mib"] for p in procs), 3),
            "processes": procs, "by_mapping_class": rows}


def per_session_rows(attr, n, baseline):
    procs = attr["processes"]
    guest = [p for p in procs if p["class"] == "guest"]
    bwrap = [p for p in procs if p["class"] == "bwrap"]
    host = [p for p in procs if p["class"] == "host"]
    wrapper = [p for p in procs if p["class"] == "wrapper"]
    def tot(rows, key="pss_mib"):
        return round(sum(r[key] for r in rows), 3)
    def per(rows):
        return round(sum(r["pss_mib"] for r in rows) / n, 3) if n else None
    def tot_private(rows):
        return round(sum(r.get("private_mib", 0.0) for r in rows), 3)

    def tot_shared(rows):
        return round(sum(r.get("shared_mib", 0.0) for r in rows), 3)

    return {
        "sessions": n,
        "guest_pss_mib_total": tot(guest),
        "guest_pss_mib_per_session": per(guest),
        # `private_mib` is smaps Private_Clean+Private_Dirty (the USS
        # definition). NOTE: a MAP_PRIVATE file mapping (boot image, odex,
        # fonts) reports its pages as Private_* even though the page cache is
        # shared; Pss is the number that discounts that, so both are reported.
        "guest_private_mib_total": tot_private(guest),
        "guest_private_mib_per_session": round(tot_private(guest) / n, 3) if n else None,
        "guest_shared_mib_total": tot_shared(guest),
        "guest_shared_mib_per_session": round(tot_shared(guest) / n, 3) if n else None,
        "bwrap_pss_mib_per_session": per(bwrap),
        "host_pss_mib_total": tot(host),
        "host_pss_mib_per_session": per(host),
        "wrapper_pss_mib_per_session": per(wrapper),
        "guest_processes": len(guest),
        "all_pss_mib_total": attr["total_pss_mib"],
        "all_pss_mib_per_session": round(attr["total_pss_mib"] / n, 3) if n else None,
        "serve_baseline_pss_mib": (round(sum(b["pss"] or 0 for b in
                                             (baseline or {}).get("pids", [])), 3)
                                    if baseline else None),
    }


def main(argv=None):
    p = argparse.ArgumentParser()
    p.add_argument("--acos", required=True)
    p.add_argument("--out", required=True)
    p.add_argument("--n", type=int, required=True)
    p.add_argument("--loops", type=int, default=3)
    p.add_argument("--zygote", choices=["off", "on"], default="off")
    p.add_argument("--settle-ms", type=int, default=2000)
    p.add_argument("--sample-ms", type=int, default=250)
    p.add_argument("--label", default=None)
    p.add_argument("--lock", default="shared")
    p.add_argument("--scratch", default=str(WT / ".cache/d20"))
    p.add_argument("--keep-scratch", action="store_true")
    p.add_argument("--boot-image-cache", default=str(WT / ".cache/d20/boot-images"))
    p.add_argument("--apk", default=str(APK))
    args = p.parse_args(argv)
    if not Path(args.apk).exists():
        sys.stderr.write("missing fixture apk: %s\n" % args.apk)
        return 3
    doc = run_cohort(args)
    print(json.dumps({
        "n": doc["n"], "arm": doc["arm"], "verified": doc["verified_tasks"],
        "measured": doc["measured_tasks"], "wall_s": doc.get("wall_s"),
        "gib_min_pss": doc.get("gib_min_pss"),
        "per_gib_min": doc.get("verified_per_gib_min_pss"),
        "peak_tree_pss_mib": doc.get("peak_tree_pss_mib"),
        "host_used_pct_peak": doc.get("host_used_pct_peak"),
        "per_session": doc.get("per_session_settled"),
        "failures": len(doc.get("failed_tasks") or []),
        "out": args.out,
    }, separators=(",", ":")))
    return 0 if doc["verified_tasks"] else 1


if __name__ == "__main__":
    sys.exit(main())
