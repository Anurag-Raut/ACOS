#!/usr/bin/env bash
# mem-breakdown.sh -- trustworthy memory measurement for ONE agent task (W41).
#
# Why this exists (W34 §6.7): `tools/run-suite.sh` samples `memory.current` of
# the cgroup the *runner* is in. Two problems make that denominator unusable:
#
#   1. SCOPE. `acos-art` creates the guest's cgroup with `systemd-run --scope`,
#      which lands as a *sibling* scope under app.slice, not as a child of the
#      job scope. The job scope therefore does not contain the guest at all:
#      run-suite's "job GiB.min" is python + acos + host-side page cache only.
#   2. COMPOSITION. cgroup v2 charges shared read-only page cache (the 860 MiB
#      ART sysroot, the 63 MiB debug `acos` binary, PNG/verdict writes) to
#      whichever cgroup faults it in first. That is not the task's cost; it
#      makes the same binary swing >1.7x depending on what another live cgroup
#      already cached.
#
# This script samples, every 100 ms, for one task:
#   * the job cgroup (the scope the runner is in): memory.current + memory.stat
#     (anon, file, shmem, kernel, sock, ...);
#   * every guest/child cgroup the run creates (discovered from the launcher's
#     own `<run>/cgroup` evidence file and from sibling `run-*.scope` roots that
#     hold our descendants);
#   * per-process PSS/RSS (guest dalvikvm64, acos-art + bwrap, host acos).
# and reports integrals (GiB.min) and peaks for each *component* and each
# *phase* (boot | launch | steps | teardown), plus two denominators:
#
#   scope_gib_min   = integral(memory.current of the job scope)  -- run-suite's number
#   job_gib_min     = integral(sum over job + guest cgroups memory.current)
#   job_res_gib_min = integral(sum over job + guest of anon+shmem+kernel+sock)
#                     (page cache excluded: the metric to gate M11 on)
#
# It never edits run-suite.sh/benchlib.py. Usage (one ART run per lock):
#
#   tools/art-runtime/run-exclusive.sh --task W41 -- \
#     tools/guarded.sh --mem 8G --time 900 -- \
#     tools/mem-breakdown.sh --task T07 --reps 1 \
#       --out results/bench/mem-T07-$(date -u +%Y%m%dT%H%M%SZ).json
#
# Options:
#   --task ID|PATH   T01..T12 (good twin from scenarios/suite/INDEX.yaml) or a
#                    scenario path (default T07)
#   --reps N         repetitions in this process (default 1; repeatability needs
#                    fresh *scopes*, so call it once per guarded invocation)
#   --acos PATH      acos binary (default $ACOS_SUITE_ACOS, then target/debug)
#   --acos-art PATH  launcher binary (default $ACOS_ART_BIN, then target/debug)
#   --env K=V        extra env for the task (repeatable) -- experiments
#   --label TEXT     label stored in the JSON (e.g. "baseline", "heap-96m")
#   --out FILE       JSON output (default results/bench/mem-<UTC>.json)
#   --interval SECS  sampling interval (default 0.1)
#   --no-host-gate   skip benchlib.host_gate (the caller may already own the lock)
#
# Exit: 0 = task passed and a measurement was written; 1 = task did not pass
# (the measurement is still written, verdict recorded); 2 = setup error.
set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
export ACOS_MEM_ROOT="$ROOT"
exec python3 -I - "$@" <<'PY'
import datetime
import json
import os
import re
import subprocess
import sys
import threading
import time
from pathlib import Path

ROOT = Path(os.environ["ACOS_MEM_ROOT"])
sys.path.insert(0, str(ROOT / "tools"))
import benchlib  # noqa: E402  (stats, host gate, provenance, integrals)

SUITE = ROOT / "scenarios/suite"
BENCH = ROOT / "results/bench"
SCRATCH = ROOT / ".cache/w41-mem"
MiB = 1 << 20
GiB = 1 << 30


def utc_now():
    return datetime.datetime.now(datetime.timezone.utc).isoformat(timespec="seconds")


def stamp():
    return datetime.datetime.now(datetime.timezone.utc).strftime("%Y%m%dT%H%M%SZ")


def die(msg, code=2):
    print("mem-breakdown: " + msg, file=sys.stderr)
    raise SystemExit(code)


def parse(argv):
    o = {"task": "T07", "reps": 1, "acos": None, "acos_art": None, "env": [],
         "label": "baseline", "out": None, "interval": 0.1, "host_gate": True}
    i = 0
    while i < len(argv):
        a = argv[i]
        if a in ("--task", "--reps", "--acos", "--acos-art", "--env", "--label",
                 "--out", "--interval"):
            if i + 1 >= len(argv):
                die(f"{a} needs a value")
            v = argv[i + 1]
            if a == "--env":
                o["env"].append(v)
            elif a == "--reps":
                o["reps"] = int(v)
            elif a == "--interval":
                o["interval"] = float(v)
            else:
                o[a.lstrip("-").replace("-", "_")] = v
            i += 2
        elif a == "--no-host-gate":
            o["host_gate"] = False
            i += 1
        elif a in ("-h", "--help"):
            print(Path(__file__).read_text().split("set -uo")[0])
            raise SystemExit(0)
        else:
            die(f"unknown argument {a!r}")
    if not (0.02 <= o["interval"] <= 5.0):
        die("--interval must be 0.02..5.0 s")
    return o


def scenario_path(task):
    p = Path(task)
    if "/" in task or p.suffix in (".yaml", ".yml"):
        return (ROOT / task) if not p.is_absolute() else p
    import yaml
    index = yaml.safe_load((SUITE / "INDEX.yaml").read_text())
    for t in index["tasks"]:
        if t["id"] == task:
            if t.get("status") == "UNSUPPORTED":
                die(f"{task} is UNSUPPORTED (no scenario)")
            return SUITE / t["good"]["scenario"]
    die(f"unknown task {task!r}")


# --------------------------------------------------------------------------- #
# cgroup discovery and sampling
# --------------------------------------------------------------------------- #
def self_cgroup():
    try:
        for line in Path("/proc/self/cgroup").read_text().splitlines():
            if line.startswith("0::"):
                rel = line[3:].strip()
                return Path("/sys/fs/cgroup") / rel.lstrip("/") if rel else None
    except OSError:
        pass
    return None


def cg_from_info_file(path):
    """The launcher's evidence file: a `/proc/self/cgroup` snapshot of the guest scope."""
    try:
        for line in path.read_text().splitlines():
            if line.startswith("0::"):
                rel = line[3:].strip()
                if rel and rel != "/":
                    return Path("/sys/fs/cgroup") / rel.lstrip("/")
    except OSError:
        pass
    return None


def read_int(path, name):
    try:
        return int((path / name).read_text().strip())
    except (OSError, ValueError):
        return None


STAT_KEYS = ("anon", "file", "shmem", "kernel", "kernel_stack", "sock",
             "slab", "pagetables", "pgfault", "pgmajfault", "workingset_refault_file")


def read_stat(path):
    out = {}
    try:
        for line in (path / "memory.stat").read_text().splitlines():
            k, _, v = line.partition(" ")
            if k in STAT_KEYS:
                try:
                    out[k] = int(v)
                except ValueError:
                    pass
    except OSError:
        return None
    return out


def pss_of(pid):
    """(comm, Pss_kb, Rss_kb) from /proc/<pid>/smaps_rollup (fallback status)."""
    try:
        comm = Path(f"/proc/{pid}/comm").read_text().strip()
    except OSError:
        return None
    pss = rss = None
    try:
        for line in Path(f"/proc/{pid}/smaps_rollup").read_text().splitlines():
            if line.startswith("Pss:"):
                pss = int(line.split()[1])
            elif line.startswith("Rss:"):
                rss = int(line.split()[1])
            if pss is not None and rss is not None:
                break
    except (OSError, ValueError, IndexError):
        pass
    if pss is None:
        try:
            pss = int(next(l for l in Path(f"/proc/{pid}/status").read_text().splitlines()
                           if l.startswith("VmRSS:")).split()[1])
            rss = pss
        except (OSError, StopIteration, ValueError):
            return None
    return comm, pss, rss


def classify(comm, tier="job"):
    if tier == "guest":
        # The guest's binder process is named after the app (e.g.
        # ".formvalidation"); a plain dalvikvm64 is the runtime itself.
        return "guest-dalvikvm" if comm.startswith("dalvikvm") else "guest-app"
    if comm.startswith("dalvikvm"):
        return "guest-dalvikvm"
    if comm.startswith("acos-art"):
        return "acos-art-launcher"
    if comm.startswith("acos"):
        return "acos-host"
    if comm == "bwrap":
        return "bwrap"
    if comm.startswith("python"):
        return "sampler-python"
    if comm in ("sh", "timeout", "systemd-run", "cat", "sed"):
        return "harness-shell"
    return "other"


def procs_in_cgroup(path):
    try:
        return [int(x) for x in (path / "cgroup.procs").read_text().split()]
    except (OSError, ValueError):
        return []


class Sampler(threading.Thread):
    """100 ms sampler of the job scope plus every guest scope the run creates."""

    def __init__(self, job, stop, interval, info_dirs, pss_every=3):
        super().__init__(daemon=True)
        self.job = job
        self.stop = stop
        self.interval = interval
        self.info_dirs = [Path(d) for d in info_dirs if d]
        self.pss_every = pss_every
        self.samples = []          # job scope
        self.extra = {}            # str(cg) -> [{"t","bytes","stat"}]
        self.pss = []              # [{"t","procs":[{class,comm,pid,pss_kb,rss_kb}]}]
        self.discovered = set()
        self.misses = 0
        self.ticks = 0

    def _discover(self):
        found = set()
        for d in self.info_dirs:
            if not d.exists():
                continue
            try:
                for p in d.rglob("cgroup"):
                    cg = cg_from_info_file(p)
                    if cg is not None and cg != self.job and cg.exists():
                        found.add(cg)
            except OSError:
                continue
        # Fallback: our own descendants that live in another scope (covers a
        # launcher that has not yet written its evidence file this tick).
        # Descendant-ness is checked by the ppid chain: other users of bwrap on
        # this host (flatpak apps) must never be charged to this job.
        me = os.getpid()
        ppid = {}
        for p in Path("/proc").iterdir():
            if not p.name.isdigit():
                continue
            try:
                # /proc/<pid>/stat: "pid (comm) state ppid ..." -- comm may
                # contain spaces/parens, so split after the last ')'.
                raw = (p / "stat").read_text()
                tail = raw[raw.rfind(")") + 1:].split()
                ppid[int(p.name)] = int(tail[1])
            except (OSError, ValueError, IndexError):
                continue

        def is_descendant(pid):
            hops = 0
            while pid and hops < 64:
                if pid == me:
                    return True
                pid = ppid.get(pid)
                hops += 1
            return False

        for p in Path("/proc").iterdir():
            if not p.name.isdigit() or not is_descendant(int(p.name)):
                continue
            try:
                comm = (p / "comm").read_text().strip()
            except OSError:
                continue
            try:
                for line in (p / "cgroup").read_text().splitlines():
                    if line.startswith("0::"):
                        rel = line[3:].strip()
                        cg = Path("/sys/fs/cgroup") / rel.lstrip("/")
                        if cg != self.job and cg.exists():
                            found.add(cg)
            except OSError:
                continue
        self.discovered |= found
        return found

    def run(self):
        tick = 0
        while not self.stop.is_set():
            t = time.time()
            self.ticks += 1
            cur = read_int(self.job, "memory.current")
            if cur is None:
                self.misses += 1
            else:
                self.samples.append({"t": t, "bytes": cur, "stat": read_stat(self.job)})
            for cg in self._discover():
                c = read_int(cg, "memory.current")
                if c is not None:
                    self.extra.setdefault(str(cg), []).append(
                        {"t": t, "bytes": c, "stat": read_stat(cg)})
            if tick % self.pss_every == 0:
                cgs = [(self.job, "job")] + [(Path(x), "guest")
                                             for x in self.extra]
                procs = []
                seen = set()
                for cg, tier in cgs:
                    for pid in procs_in_cgroup(cg):
                        if pid in seen:
                            continue
                        seen.add(pid)
                        r = pss_of(pid)
                        if r:
                            comm, pss, rss = r
                            procs.append({"class": classify(comm, tier), "comm": comm,
                                          "pid": pid, "pss_kb": pss, "rss_kb": rss})
                self.pss.append({"t": t, "procs": procs})
            tick += 1
            left = self.interval - (time.time() - t)
            if left > 0:
                time.sleep(left)


def series_integral(rows, key="bytes", t0=None, t1=None):
    sel = []
    for r in rows:
        t = r["t"]
        if t0 is not None and t < t0:
            continue
        if t1 is not None and t > t1:
            continue
        v = r.get(key) if key != "bytes" else r.get("bytes")
        if v is not None:
            sel.append({"t": t, "bytes": v})
    return benchlib.sample_integral(sel) if len(sel) >= 2 else None


def stat_integral(rows, keys, t0=None, t1=None):
    """Integral of the sum of the named memory.stat keys."""
    sel = []
    for r in rows:
        if t0 is not None and r["t"] < t0:
            continue
        if t1 is not None and r["t"] > t1:
            continue
        st = r.get("stat") or {}
        if not st:
            continue
        sel.append({"t": r["t"], "bytes": sum(st.get(k, 0) for k in keys)})
    return benchlib.sample_integral(sel) if len(sel) >= 2 else None


def peak_of(rows, key=None, keys=None):
    vals = []
    for r in rows:
        if keys:
            st = r.get("stat") or {}
            vals.append(sum(st.get(k, 0) for k in keys))
        else:
            vals.append(r.get("bytes"))
    vals = [v for v in vals if v is not None]
    return max(vals) if vals else None


def mib(v):
    return round(v / MiB, 3) if v is not None else None


def gibmin(v):
    return round(v, 9) if v is not None else None


def phase_windows(t_start, verdict, session_timing):
    """boot | launch | steps | teardown windows, from the engine's own timing.

    Sources: `art-frames/session-timing.json` (session_start_ms = spawn -> ready
    line, shutdown_ms = close + process-group proof; W34) and verdict
    `steps[].duration_ms` (step 0 is `launch`). Every boundary is approximate to
    one sample; the declared geometry is boot+launch+steps+teardown ==
    resources.wall_ms + session_start_ms + shutdown_ms within one sample.
    """
    timing = session_timing or {}
    boot = (timing.get("session_start_ms") or 0) / 1000.0
    shut = (timing.get("shutdown_ms") or 0) / 1000.0
    steps = (verdict or {}).get("steps") or []
    launch_ms = 0.0
    if steps and steps[0].get("step") == "launch":
        launch_ms = (steps[0].get("duration_ms") or 0) / 1000.0
    body = sum((s.get("duration_ms") or 0) for s in steps) / 1000.0
    wall = ((verdict or {}).get("resources") or {}).get("wall_ms")
    body = (wall / 1000.0) if wall else body
    t_boot = t_start + boot
    t_launch = t_boot + launch_ms
    t_steps = max(t_launch, t_boot + body)
    end = t_start + boot + body + shut
    return {
        "boot": (t_start, t_boot),
        "launch": (t_boot, t_launch),
        "steps": (t_launch, t_steps),
        "teardown": (t_steps, end),
        "boot_ms": round(boot * 1000, 1),
        "launch_ms": round(launch_ms * 1000, 1),
        "steps_ms": round(max(0.0, body - launch_ms) * 1000, 1),
        "teardown_ms": round(shut * 1000, 1),
    }


def one_rep(task, scenario, opts, acos, acos_art, rep, extra_env):
    art = SCRATCH / stamp() / Path(scenario).stem / f"r{rep}"
    art.mkdir(parents=True, exist_ok=True)
    env = dict(os.environ)
    env.update(extra_env)
    env["ACOS_ART_BIN"] = acos_art
    cmd = [acos, "verify", str(scenario), "--engine", "art", "--json",
           "--artifacts", str(art)]
    stop = threading.Event()
    job = self_cgroup()
    if job is None:
        die("cannot resolve this process's cgroup (no cgroup v2 0:: entry)")
    info_dirs = [env.get("ACOS_ART_DATA"), str(SCRATCH)]
    sampler = Sampler(job, stop, opts["interval"], info_dirs)
    sampler.start()
    t_start = time.time()
    proc = subprocess.run(cmd, cwd=str(ROOT), env=env, capture_output=True, text=True,
                          timeout=900)
    t_end = time.time()
    # One closing sample so the teardown phase has two points even though the
    # guest scope is reaped as the task exits.
    try:
        t = time.time()
        cur = read_int(job, "memory.current")
        if cur is not None:
            sampler.samples.append({"t": t, "bytes": cur, "stat": read_stat(job)})
        for cg in list(sampler.extra):
            c = read_int(Path(cg), "memory.current")
            if c is not None:
                sampler.extra[cg].append({"t": t, "bytes": c,
                                          "stat": read_stat(Path(cg))})
    except OSError:
        pass
    stop.set()
    sampler.join(timeout=5)
    verdict = engine = session_timing = {}
    for name, dst in (("verdict.json", verdict), ("engine.json", engine),
                      ("art-frames/session-timing.json", session_timing)):
        f = art / name
        if f.exists():
            try:
                dst.update(json.loads(f.read_text()))
            except json.JSONDecodeError:
                pass
    # Unify job + guest series into a per-timestamp sum (nearest sample is fine:
    # every series is sampled on the same tick).
    extra_series = []
    for cg, rows in sampler.extra.items():
        extra_series.append({"cgroup": cg, "n": len(rows),
                             "gib_min": gibmin(series_integral(rows)),
                             "peak_mib": mib(peak_of(rows)),
                             "anon_gib_min": gibmin(stat_integral(rows, ("anon",))),
                             "file_gib_min": gibmin(stat_integral(rows, ("file",))),
                             "res_gib_min": gibmin(stat_integral(
                                 rows, ("anon", "shmem", "kernel", "sock")))})
    total_rows = []
    for i, s in enumerate(sampler.samples):
        total = s["bytes"]
        st = dict(s.get("stat") or {})
        for cg, rows in sampler.extra.items():
            if i < len(rows):
                total += rows[i]["bytes"]
                rst = rows[i].get("stat") or {}
                for k in ("anon", "file", "shmem", "kernel", "sock"):
                    st[k] = st.get(k, 0) + rst.get(k, 0)
        total_rows.append({"t": s["t"], "bytes": total, "stat": st})
    keep = ("anon", "file", "shmem", "kernel", "sock")
    phases = phase_windows(t_start, verdict, session_timing)
    comp = {}
    for keys, label in (
            (("anon",), "anon"),
            (("file",), "file_incl_shmem"),
            (("file", "shmem"), "file_excl_shmem"),   # file is a superset of shmem
            (("shmem",), "shmem"),
            (("kernel",), "kernel"),
            (("sock",), "sock"),
            (("anon", "shmem", "kernel", "sock"), "resident_no_file")):
        if label == "file_excl_shmem":
            # file - shmem, using the paired samples
            sel = [{"t": r["t"], "bytes": max(0, r["stat"].get("file", 0) -
                                              r["stat"].get("shmem", 0))}
                   for r in total_rows if r["stat"]]
            comp[label] = {"gib_min": gibmin(benchlib.sample_integral(sel)) if len(sel) > 1 else None,
                           "peak_mib": mib(max((x["bytes"] for x in sel), default=None))}
            continue
        comp[label] = {
            "gib_min": gibmin(stat_integral(total_rows, keys)),
            "peak_mib": mib(peak_of(total_rows, keys=keys)),
        }
    phase_tbl = {}
    for name in ("boot", "launch", "steps", "teardown"):
        w0, w1 = phases[name]
        phase_tbl[name] = {
            "seconds": round(w1 - w0, 3),
            "scope_gib_min": gibmin(series_integral(sampler.samples, t0=w0, t1=w1)),
            "job_gib_min": gibmin(series_integral(total_rows, t0=w0, t1=w1)),
            "resident_no_file_gib_min": gibmin(
                stat_integral(total_rows, ("anon", "shmem", "kernel", "sock"),
                              t0=w0, t1=w1)),
            "peak_mib": mib(peak_of([r for r in total_rows if w0 <= r["t"] <= w1])),
        }
    # per-process PSS at the tick where the job+guest total peaked
    pss_max = {}
    pss_peak_tick = None
    if sampler.samples and sampler.pss:
        peak_i = max(range(len(sampler.samples)),
                     key=lambda i: sampler.samples[i]["bytes"])
        pss_peak_tick = min(sampler.pss,
                            key=lambda p: abs(p["t"] - sampler.samples[peak_i]["t"]))
    for p in sampler.pss:
        for row in p["procs"]:
            cur = pss_max.setdefault(row["class"], {"pss_mib": 0, "rss_mib": 0,
                                                    "comm": row["comm"], "n_procs": 0})
            cur["pss_mib"] = max(cur["pss_mib"], round(row["pss_kb"] / 1024.0, 3))
            cur["rss_mib"] = max(cur["rss_mib"], round(row["rss_kb"] / 1024.0, 3))
            cur["n_procs"] = max(cur["n_procs"], 1)
    self_rss = next((s for s in sampler.samples), None)
    self_mib = None
    try:
        for line in Path("/proc/self/status").read_text().splitlines():
            if line.startswith("VmRSS:"):
                self_mib = round(int(line.split()[1]) / 1024.0, 2)
    except (OSError, ValueError):
        pass
    scope_gib = gibmin(series_integral(sampler.samples))
    job_gib = gibmin(series_integral(total_rows))
    res_gib = gibmin(stat_integral(total_rows, ("anon", "shmem", "kernel", "sock")))
    # The sampler itself lives in the job scope (24 MiB anon). Charge it out so a
    # caller can see the task's own cost; the raw values above still include it.
    self_gib = (round((self_mib or 0) / 1024.0 * (t_end - t_start) / 60.0, 9)
                if self_mib else None)
    st = (verdict.get("resources") or {}).get("wall_ms")
    return {
        "rep": rep,
        "scenario": str(scenario.relative_to(ROOT)) if str(scenario).startswith(str(ROOT))
                    else str(scenario),
        "utc": utc_now(),
        "label": opts["label"],
        "env": extra_env,
        "verdict": verdict.get("verdict"),
        "error": (((verdict.get("steps") or [{}])[-1] or {}).get("error")
                  if verdict.get("verdict") != "pass" else None),
        "rc": proc.returncode,
        "process_wall_s": round(t_end - t_start, 3),
        "verdict_wall_ms": st,
        "job_cgroup": str(job),
        "guest_cgroups": sorted(sampler.extra),
        "samples": {"ticks": sampler.ticks, "job": len(sampler.samples),
                    "misses": sampler.misses, "pss_ticks": len(sampler.pss),
                    "interval_s": opts["interval"]},
        "self_rss_mib": self_mib,
        "phase": phase_tbl,
        "denominator": {
            "scope_gib_min_runner_formula": scope_gib,
            "job_gib_min_all_cgroups": job_gib,
            "job_resident_no_file_gib_min": res_gib,
            "job_peak_mib_all_cgroups": mib(peak_of(total_rows)),
            "sampler_overhead_gib_min": self_gib,
            "job_resident_excl_sampler_gib_min":
                round(res_gib - self_gib, 9) if (res_gib is not None and self_gib) else res_gib,
            "scope_peak_mib_runner_formula": mib(peak_of(sampler.samples)),
        },
        "components": comp,
        "guest_series": extra_series,
        "pss_peak_mib_by_class": pss_max,
        "pss_at_peak_tick": ([{"class": r["class"], "comm": r["comm"],
                               "pid": r["pid"], "pss_mib": round(r["pss_kb"] / 1024.0, 3),
                               "rss_mib": round(r["rss_kb"] / 1024.0, 3)}
                              for r in pss_peak_tick["procs"]] if pss_peak_tick else []),
        "artifacts": str(art),
        "rc_stdout_tail": (proc.stdout or "")[-800:] if proc.returncode not in (0, 1) else None,
        "stderr_tail": (proc.stderr or "")[-1500:] if proc.returncode != 0 else None,
    }


def aggregate(paths):
    """Print/emit one row per file: the same fields, with benchlib.stats."""
    sys.path.insert(0, str(ROOT / "tools"))
    import benchlib
    rows = []
    for p in paths:
        try:
            d = json.loads(Path(p).read_text())
        except (OSError, json.JSONDecodeError):
            continue
        for r in d.get("reps", []):
            rows.append({
                "file": p, "task": d.get("task"), "label": d.get("label"),
                "env": d.get("env"), "rep": r["rep"], "verified": r["verdict"],
                "scope_gib_min": r["denominator"]["scope_gib_min_runner_formula"],
                "job_gib_min": r["denominator"]["job_gib_min_all_cgroups"],
                "job_resident_gib_min": r["denominator"]["job_resident_no_file_gib_min"],
                "job_peak_mib": r["denominator"]["job_peak_mib_all_cgroups"],
                "anon_gib_min": r["components"]["anon"]["gib_min"],
                "file_gib_min": r["components"]["file_incl_shmem"]["gib_min"],
                "shmem_gib_min": r["components"]["shmem"]["gib_min"],
                "kernel_gib_min": r["components"]["kernel"]["gib_min"],
                "process_wall_s": r["process_wall_s"],
            })
    fields = ("scope_gib_min", "job_gib_min", "job_resident_gib_min", "job_peak_mib",
              "anon_gib_min", "file_gib_min", "shmem_gib_min", "kernel_gib_min",
              "process_wall_s")
    groups = {}
    for x in rows:
        groups.setdefault((x["task"], x["label"],
                           json.dumps(x.get("env") or {}, sort_keys=True)), []).append(x)
    stats = {}
    for key, rs in sorted(groups.items()):
        stats["|".join(key)] = {
            "task": key[0], "label": key[1], "env": key[2], "n_rows": len(rs),
            "verified": sum(1 for x in rs if x["verified"] == "pass"),
            "rows": rs,
            "stats": {f: benchlib.stats([x[f] for x in rs if x.get(f) is not None])
                      for f in fields}}
    out = {"schema": "acos-mem-breakdown-aggregate/2", "utc": utc_now(),
           "rows": rows, "groups": stats}
    print("%-6s %-10s %-30s %3s %-10s %-10s %-10s %-9s %-9s %-8s" % (
        "task", "label", "env", "n", "scope", "job_all", "resident", "anon",
        "file", "peakMiB"))
    for key, g in stats.items():
        st = g["stats"]
        def p50(f):
            v = st[f]["p50"]
            return ("%.5f" % v) if v is not None else "-"
        print("%-6s %-10s %-30s %3d %-10s %-10s %-10s %-9s %-9s %-8s" % (
            g["task"], g["label"], (g["env"] or "none")[:30], g["n_rows"],
            p50("scope_gib_min"), p50("job_gib_min"), p50("job_resident_gib_min"),
            p50("anon_gib_min"), p50("file_gib_min"), p50("job_peak_mib")))
        if g["n_rows"] >= 2:
            for f in ("job_resident_gib_min", "job_gib_min", "scope_gib_min"):
                r = st[f]
                print("       %-22s n=%d spread=%s ci95=%s..%s" % (
                    f, r["n"], ("%.1f%%" % r["spread_pct"]) if r["spread"] is not None
                    else "n/a", r["ci95_low"], r["ci95_high"]))
    return out


def main():
    argv = sys.argv[1:]
    if argv and argv[0] == "--aggregate":
        out = aggregate(argv[1:])
        path = BENCH / f"mem-W41-aggregate-{stamp()}.json"
        path.parent.mkdir(parents=True, exist_ok=True)
        benchlib.write_json(path, out)
        print("wrote " + str(path))
        raise SystemExit(0)
    opts = parse(sys.argv[1:])
    scenario = scenario_path(opts["task"])
    if not scenario.exists():
        die(f"missing scenario {scenario}")
    acos = opts["acos"] or os.environ.get("ACOS_SUITE_ACOS") or str(ROOT / "target/debug/acos")
    acos_art = (opts["acos_art"] or os.environ.get("ACOS_ART_BIN")
                or str(ROOT / "target/debug/acos-art"))
    for b in (acos, acos_art):
        if not Path(b).exists():
            die(f"binary not found: {b}")
    extra_env = {}
    for kv in opts["env"]:
        k, sep, v = kv.partition("=")
        if not sep:
            die(f"--env expects K=V, got {kv!r}")
        extra_env[k] = v
    inside_lock = bool(os.environ.get("ACOS_ART_DATA"))
    gate = (benchlib.host_gate(ROOT, check_lock=not inside_lock)
            if opts["host_gate"] else None)
    if gate and not gate["ok"]:
        print("mem-breakdown: WARNING host gate: " + "; ".join(gate["reasons"]),
              file=sys.stderr)
    reps = []
    for r in range(1, opts["reps"] + 1):
        rep = one_rep(opts["task"], scenario, opts, acos, acos_art, r, extra_env)
        if rep["rc"] != 0 or rep["verdict"] != "pass":
            # Observed on this host (W41): a fresh `systemd-run --user --scope`
            # created back-to-back with a retiring one can fail before the guest
            # starts (session_start_ms=0, rc=1). Retry once, 2 s apart, and keep
            # the failed attempt in the record rather than hiding it.
            first = {k: rep.get(k) for k in ("rc", "verdict", "utc", "stderr_tail")}
            time.sleep(2)
            rep = one_rep(opts["task"], scenario, opts, acos, acos_art, r, extra_env)
            rep["retried_after"] = first
        reps.append(rep)
    fields = {
        "scope_gib_min": [r["denominator"]["scope_gib_min_runner_formula"] for r in reps],
        "job_gib_min": [r["denominator"]["job_gib_min_all_cgroups"] for r in reps],
        "job_resident_gib_min": [r["denominator"]["job_resident_no_file_gib_min"]
                                 for r in reps],
        "job_peak_mib": [r["denominator"]["job_peak_mib_all_cgroups"] for r in reps],
        "process_wall_s": [r["process_wall_s"] for r in reps],
        "anon_gib_min": [r["components"]["anon"]["gib_min"] for r in reps],
        "file_gib_min": [r["components"]["file_incl_shmem"]["gib_min"] for r in reps],
        "shmem_gib_min": [r["components"]["shmem"]["gib_min"] for r in reps],
        "kernel_gib_min": [r["components"]["kernel"]["gib_min"] for r in reps],
    }
    summary = {k: benchlib.stats([v for v in vs if v is not None]) for k, vs in fields.items()}
    passed = [r for r in reps if r["verdict"] == "pass"]
    out = {
        "schema": "acos-mem-breakdown/1",
        "utc_start": reps[0]["utc"] if reps else utc_now(),
        "retried": [r["rep"] for r in reps if r.get("retried_after")],
        "utc_finish": utc_now(),
        "task": opts["task"],
        "label": opts["label"],
        "scenario": reps[0]["scenario"] if reps else None,
        "engine": "art",
        "binaries": {"acos": acos, "acos_art": acos_art},
        "env": extra_env,
        "host_gate": gate,
        "verified": f"{len(passed)}/{len(reps)}",
        "reps": reps,
        "summary": summary,
        "method": {
            "interval_s": opts["interval"],
            "job_cgroup": "the cgroup this process runs in (guarded.sh scope)",
            "guest_cgroups": "discovered from the launcher's <run>/cgroup evidence "
                             "file and from dalvikvm/bwrap /proc/<pid>/cgroup; "
                             "systemd-run --scope puts them OUTSIDE the job scope",
            "scope_gib_min_runner_formula": "run-suite.sh's number: job scope only",
            "job_gib_min_all_cgroups": "job scope + guest scopes, memory.current",
            "job_resident_no_file_gib_min": "job + guest, sum(anon,shmem,kernel,sock): "
                                            "shared page cache excluded",
            "phase_windows": "engine.json.timing session_start_ms / shutdown_ms; "
                             "approximate to one sample",
        },
    }
    path = Path(opts["out"]) if opts["out"] else (
        BENCH / f"mem-{opts['task']}-{stamp()}.json")
    path.parent.mkdir(parents=True, exist_ok=True)
    benchlib.write_json(path, out)
    print(f"mem-breakdown: {opts['task']} {opts['label']} verified={out['verified']} "
          f"scope={summary['scope_gib_min']['p50']} "
          f"job_all={summary['job_gib_min']['p50']} "
          f"job_resident={summary['job_resident_gib_min']['p50']} GiB.min")
    print(f"spread: scope={summary['scope_gib_min']['spread']} "
          f"job_all={summary['job_gib_min']['spread']} "
          f"job_resident={summary['job_resident_gib_min']['spread']}")
    print(f"wrote {path}")
    raise SystemExit(0 if len(passed) == len(reps) else 1)


main()
PY
