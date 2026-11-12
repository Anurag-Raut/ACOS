#!/usr/bin/env bash
# run-suite.sh -- run the frozen agent-task suite T01-T12 (scenarios/suite/) on
# the real ART engine, sequentially or N variant runs at once, and write
# results/bench/suite-<UTCstamp>.json.
#
# Correctness contract (advisor item 2):
#   * every CORRECT variant must reach verdict pass (strict);
#   * every WRONG variant must reach verdict fail with the expected structured
#     error code (ASSERTION_FAILED / ELEMENT_NOT_FOUND / ...);
#   * false_pass_count counts wrong twins that PASSED. Any false pass => the
#     main metric M11 is zero (advisor item 3) and this script exits 1;
#   * tasks that cannot be expressed are recorded as named UNSUPPORTED (never
#     skipped silently, never faked).
#
# Memory measurement contract (W45; fixes the W41 denominator defects):
#   * ONE FRESH GUARDED SCOPE PER VARIANT RUN. Every run is wrapped in
#     `tools/guarded.sh` (a new transient systemd scope), and the sampler runs
#     *inside* that scope, so the suite-lifetime scope (W41 1.3) cannot exist.
#     A duplicated job cgroup across runs is a hard measurement defect: M11 is
#     null and the run is not accepted.
#   * The sampler (`python3 tools/benchlib.py sample-scope`) samples the job
#     cgroup AND every guest cgroup `acos-art` creates (a sibling scope, W41 1.1)
#     every 100 ms, with a closing sample after the child exits.
#   * PRIMARY DENOMINATOR = cgroup `memory.stat` anon+shmem, job+guest, sampler
#     excluded. It is the ONLY number the bar is evaluated on. NOTE (W52): the
#     24.4 bar's own inputs are NOT all this type (206 MiB anon+shmem core +
#     112.1 MiB memory.current marginal + a 19 s warm dev-loop wall), so the
#     cross-type comparison is reported NOT_COMPARABLE; see BENCHMARKS.md 12.
#   * Also recorded, each labelled `gate_uses: false`: `resident`
#     (anon+shmem+kernel), `job_all_incl_file` (memory.current, includes page
#     cache), `scope_runner_formula` (the pre-W45 job-scope-only number) and
#     `infra_file_cache` (shared read-only page cache, reported separately and
#     never charged to a task).
#   * The sampler's own anon+shmem x window is subtracted from every denominator.
#   * W52 --concurrency N: up to N variant runs at once, each in its OWN fresh
#     guarded scope with its own sampler (the W45 contract is unchanged). The
#     shared ACOS_ART_DATA evidence tree is filtered by launcher-pid ownership so
#     one run never charges another's guest. Denominator = the sum of the
#     per-run integrals over the same wall window. In parallel, an independent
#     host-level monitor discovers every job + guest cgroup of the window from
#     the suite's process tree and integrates their union; the two must agree
#     within the 5 % noise floor or the run is rejected as SCOPE_MISMATCH.
#   * Per-run artifact dirs are pruned after the summary JSON is written unless
#     --keep-artifacts (the host disk filled during W41). Passing runs keep a
#     small evidence subset (verdict/engine/session-timing/mem + files <= 32 KiB);
#     non-pass runs additionally keep up to 4 MiB of failure artifacts.
#
# M11 formula (advisor item 3, exact):
#   primary_gib_min = sum over 100 ms samples of (anon+shmem bytes)/2^30 * dt_s/60
#                     for the job cgroup + every guest cgroup, per variant run
#   M11             = verified_good / sum(primary_gib_min) over the suite at N=10
#                     concurrent, 3 rounds, median
#   if false_pass > 0: M11 := 0
#   legacy-derived bar:
#     ((206 MiB core / 10) + 112.1 MiB marginal) / 1024 * (19 s / 60)
#       = 0.0410 GiB*min/task  =>  24.4 tasks/GiB*min (legacy-derived-bound)
#     ACOS must exceed 24.4 * (1 + noise); noise = max(spread, 5 %).
# This runner executes N=1 by default, so its M11 is labelled gate_eligible=false.
# --concurrency 10 --rounds 3 makes the row gate eligible (subject to the scope
# check, false_pass 0 and the host-level cross-check). The CI is a bootstrap over
# tasks (both numerator and denominator recomputed).
#
# Usage (mandated form; the whole suite inside one host-wide ART lock):
#   tools/art-runtime/run-exclusive.sh --task W52 -- \
#     tools/guarded.sh --mem 16G --time 2400 -- tools/run-suite.sh \
#       --rounds 3 --concurrency 10 --acos .cache/w52-target/release/acos \
#       --acos-art .cache/w52-target/release/acos-art --out results/bench/suite-W52.json
#
# Standalone sequential use (the script then wraps every variant in
# run-exclusive.sh itself, still one ART job at a time):
#   tools/run-suite.sh [--rounds N] [--concurrency N] [--only T01,T02]
#                      [--acos PATH] [--acos-art PATH] [--out FILE]
#                      [--timeout S] [--mem 8G] [--no-host-gate]
#                      [--keep-artifacts]
# Concurrency > 1 must run inside one run-exclusive.sh acquisition (set
# ACOS_ART_DATA); the lock is per suite, not per run.
#
# Exit: 0 = all expressible variants behaved as declared (UNSUPPORTED recorded);
#       1 = false pass, wrong-variant mismatch, a correct variant failed, or the
#           per-variant-fresh-scope rule was violated (M11 null);
#       2 = usage/host/index/prebuilt-APK problem (no verdicts written).
set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
export ACOS_SUITE_ROOT="$ROOT"
export PATH="$ROOT/target/debug:$PATH"

exec python3 -I - "$@" <<'PY'
import datetime
import hashlib
import json
import os
import subprocess
import sys
import threading
import time
from concurrent.futures import ThreadPoolExecutor, as_completed
from pathlib import Path

import yaml

ROOT = Path(os.environ["ACOS_SUITE_ROOT"])
sys.path.insert(0, str(ROOT / "tools"))
import benchlib  # noqa: E402  (stats, host gate, provenance, scope denominators)

SUITE_DIR = ROOT / "scenarios/suite"
INDEX = SUITE_DIR / "INDEX.yaml"
BENCH_DIR = ROOT / "results/bench"
DEFAULT_ARTIFACT_DIR = ROOT / ".cache/w30-suite"
LEGACY_BAR = benchlib.LEGACY_DERIVED_BAR
NOISE = benchlib.MIN_NOISE

def utc_now():
    return datetime.datetime.now(datetime.timezone.utc).isoformat(timespec="seconds")

def stamp():
    return datetime.datetime.now(datetime.timezone.utc).strftime("%Y%m%dT%H%M%SZ")

def die(message, code=2):
    print(f"run-suite: {message}", file=sys.stderr)
    raise SystemExit(code)

def parse_args(argv):
    opts = {"rounds": 1, "only": None, "acos": None, "acos_art": None,
            "out": None, "host_gate": True, "timeout": 180, "mem": "8G",
            "keep_artifacts": False, "artifacts": str(DEFAULT_ARTIFACT_DIR),
            "sampler": str(ROOT / "tools/benchlib.py"), "concurrency": 1,
            "interval": benchlib.MEMORY_SAMPLE_MS / 1000.0}
    i = 0
    while i < len(argv):
        a = argv[i]
        if a == "--rounds":
            i += 1; opts["rounds"] = int(argv[i])
        elif a == "--concurrency":
            i += 1; opts["concurrency"] = int(argv[i])
        elif a == "--only":
            i += 1; opts["only"] = {t.strip() for t in argv[i].split(",") if t.strip()}
        elif a == "--acos":
            i += 1; opts["acos"] = argv[i]
        elif a == "--acos-art":
            i += 1; opts["acos_art"] = argv[i]
        elif a == "--out":
            i += 1; opts["out"] = argv[i]
        elif a == "--timeout":
            i += 1; opts["timeout"] = int(argv[i])
        elif a == "--mem":
            i += 1; opts["mem"] = argv[i]
        elif a == "--artifacts":
            i += 1; opts["artifacts"] = argv[i]
        elif a == "--sampler":
            i += 1; opts["sampler"] = argv[i]
        elif a == "--interval":
            i += 1; opts["interval"] = float(argv[i])
        elif a == "--no-host-gate":
            opts["host_gate"] = False
        elif a == "--keep-artifacts":
            opts["keep_artifacts"] = True
        elif a in ("-h", "--help"):
            print("run-suite.sh [--rounds N] [--concurrency N] [--only T01,T02] "
                  "[--acos PATH] [--acos-art PATH] [--out FILE] [--timeout S] "
                  "[--mem 8G] [--artifacts DIR] [--sampler PATH] [--interval S] "
                  "[--no-host-gate] [--keep-artifacts]")
            raise SystemExit(0)
        else:
            die(f"unknown argument {a!r}")
        i += 1
    if not (0.02 <= opts["interval"] <= 5.0):
        die("--interval must be 0.02..5.0 s")
    if not (1 <= opts["concurrency"] <= 32):
        die("--concurrency must be 1..32")
    return opts

def sha256(path):
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()

# --------------------------------------------------------------------------- #
# one measured variant run: fresh guarded scope + in-scope sampler
# --------------------------------------------------------------------------- #
def run_measured(cmd, env, mem_json, opts, label, inside_lock, register_dir=None):
    """Run `cmd` in its own guarded scope, sampled by benchlib sample-scope.

    Returns the sampler's JSON (or {} when no measurement was produced) plus the
    command's stdout/stderr and rc. Every run gets a NEW scope: guarded.sh is
    invoked per run, never once for the suite. ``register_dir`` is the suite's
    per-round job-scope registry the host-level monitor reads (W52).
    """
    mem_json.parent.mkdir(parents=True, exist_ok=True)
    info_dirs = [str(mem_json.parent)]
    if os.environ.get("ACOS_ART_DATA"):
        info_dirs.append(os.environ["ACOS_ART_DATA"])
    sampler_cmd = ([sys.executable, "-I", str(opts["sampler"]), "sample-scope",
                    "--out", str(mem_json), "--interval", str(opts["interval"]),
                    "--label", label]
                   + [x for d in info_dirs for x in ("--info-dir", d)]
                   + (["--register-dir", str(register_dir)] if register_dir else [])
                   + ["--"] + cmd)
    wrapped = [str(ROOT / "tools/guarded.sh"), "--mem", opts["mem"], "--time",
               str(opts["timeout"]), "--"] + sampler_cmd
    if not inside_lock:
        # Standalone: one ART job at a time. Task names stay short (<=16 chars):
        # the launcher derives a unix socket path from ACOS_ART_DATA (W41 §7.2).
        short = ("W45-" + label)[:16]
        wrapped = [str(ROOT / "tools/art-runtime/run-exclusive.sh"),
                   "--task", short, "--"] + wrapped
    proc = subprocess.run(wrapped, cwd=str(ROOT), env=env, capture_output=True,
                          text=True, timeout=opts["timeout"] + 90)
    data = {}
    if mem_json.exists():
        try:
            data = json.loads(mem_json.read_text())
        except json.JSONDecodeError:
            data = {}
    stdout = ""
    stderr_tail = (proc.stderr or "")[-1500:]
    for suffix, name in ((".stdout", "stdout"), (".stderr", "stderr")):
        p = Path(str(mem_json) + suffix)
        if p.exists():
            try:
                text = p.read_text()
            except OSError:
                text = ""
            if name == "stdout":
                stdout = text
            elif proc.returncode not in (0, 1) or "sample-scope" in text:
                stderr_tail = (stderr_tail + "\n" + text)[-1500:]
    return {"data": data, "stdout": stdout, "stderr_tail": stderr_tail,
            "rc": proc.returncode, "wall_ms": int((data.get("wall_s") or 0) * 1000)}

def mem_block(data):
    """The labelled memory record for one run (every denominator, gate flags)."""
    den = (data or {}).get("denominators") or {}

    def val(entry, key="gib_min_excl_sampler"):
        return ((den.get(entry) or {}).get(key))

    comp = den.get("components") or {}
    sampler = den.get("sampler") or {}
    scope = den.get("scope") or {}
    return {
        "metric": "cgroup memory.stat anon+shmem, job+guest scopes, sampler excluded",
        "metric_type": "CGROUP_ANON_SHMEM",
        "primary_anon_shmem_gib_min": val("primary"),
        "resident_gib_min": val("resident"),
        "job_all_gib_min": val("job_all_incl_file"),
        "scope_gib_min_runner_formula": val("scope_runner_formula"),
        "infra_file_cache_gib_min": (den.get("infra_file_cache") or {}).get("gib_min"),
        "sampler_overhead_gib_min": sampler.get("overhead_gib_min"),
        "sampler_overhead_if_full_rss_gib_min": sampler.get("overhead_if_full_rss_gib_min"),
        "sampler_anon_shmem_mib": sampler.get("anon_shmem_mib"),
        "sampler_rss_mib": sampler.get("rss_mib"),
        "peak_mib_all_cgroups": comp.get("peak_mib_all_cgroups"),
        "peak_mib_primary": comp.get("peak_mib_primary"),
        "components": comp,
        "job_cgroup": scope.get("job_cgroup"),
        "sampler_pid": (data or {}).get("sampler_pid"),
        "guest_cgroups": scope.get("guest_cgroups") or [],
        "guest_scope_count": len(scope.get("guest_cgroups") or []),
        "scope_count": scope.get("scope_count"),
        "samples": (data or {}).get("samples"),
        "scope_available": (data or {}).get("scope_available"),
        "scope_limit": (data or {}).get("scope_limit"),
        "closing_sample": scope.get("closing_sample"),
        # Legacy aliases kept for readers of pre-W45 suite JSON: BOTH now carry
        # the primary anon+shmem value (not memory.current of the job scope).
        "cgroup_gib_min": val("primary"),
        "cgroup_peak_mib": comp.get("peak_mib_all_cgroups"),
    }

def verdict_error(verdict_obj, envelope):
    """The first non-pass step error becomes the variant's structured code."""
    for step in reversed(verdict_obj.get("steps", [])):
        if step.get("status") != "pass":
            err = step.get("error") or {}
            return {
                "code": err.get("code") or "UNKNOWN",
                "message": err.get("message"),
                "step": step.get("step"),
                "step_index": step.get("index"),
            }
    if envelope.get("error"):
        err = envelope["error"]
        return {"code": err.get("code") or "UNKNOWN",
                "message": err.get("message"), "step": None, "step_index": None}
    return None

def run_round(task, kind, spec, opts, env, stamp_s, inside_lock, acos_bin, rnd):
    """One measured variant run (one round) in its own fresh guarded scope.

    Safe to call concurrently: every call owns a unique artifact dir, its own
    sampler, and its own guarded scope. The only shared state is `ACOS_ART_DATA`
    (the run lock's scratch dir), which the sampler filters by launcher-pid
    ownership so another concurrent run's cgroup evidence is never charged here.
    """
    scenario_rel = spec["scenario"]
    scenario = SUITE_DIR / scenario_rel
    if not scenario.exists():
        die(f"{task['id']}-{kind}: missing scenario {scenario}")
    artifacts = Path(opts["artifacts"]) / stamp_s / f"{task['id']}-{kind}"
    artifacts.mkdir(parents=True, exist_ok=True)
    acos_art = opts["acos_art"] or env.get("ACOS_ART_BIN") or str(ROOT / "target/debug/acos-art")
    env = dict(env)
    env["ACOS_ART_BIN"] = acos_art
    run_dir = artifacts / f"r{rnd}"
    run_dir.mkdir(parents=True, exist_ok=True)
    mem_json = run_dir / "mem.json"
    cmd = [acos_bin, "verify", str(scenario), "--engine", "art", "--json",
           "--artifacts", str(run_dir)]
    label = f"{task['id']}-{kind}"
    register_dir = None
    if opts["concurrency"] > 1:
        register_dir = (Path(opts["artifacts"]) / stamp_s / "_registry" / f"r{rnd}")
        register_dir.mkdir(parents=True, exist_ok=True)
    measured = run_measured(cmd, env, mem_json, opts, label, inside_lock,
                            register_dir=register_dir)
    envelope = {}
    try:
        envelope = json.loads(measured["stdout"])
    except json.JSONDecodeError:
        pass
    verdict_file = run_dir / "verdict.json"
    verdict_obj = {}
    if verdict_file.exists():
        try:
            verdict_obj = json.loads(verdict_file.read_text())
        except json.JSONDecodeError:
            verdict_obj = {}
    verdict = verdict_obj.get("verdict")
    if verdict is None:
        # No verdict file: the run died before an evidence bundle (crash,
        # guard, unsupported engine start). Use the envelope status.
        verdict = envelope.get("status", "error")
    error = verdict_error(verdict_obj, envelope)
    # `wall_ms` is the verdict/executor wall the suite has always reported.
    # The process wall (guest spawn -> clean shutdown) is recorded next to
    # it, and `engine.json.timing` carries the per-step attribution.
    effective_wall = (verdict_obj.get("resources") or {}).get("wall_ms",
                                                              measured["wall_ms"])
    engine_timing = None
    engine_file = run_dir / "engine.json"
    if engine_file.exists():
        try:
            engine_report = json.loads(engine_file.read_text())
            engine_timing = (engine_report.get("engine") or {}).get("timing") \
                or engine_report.get("timing")
        except (json.JSONDecodeError, OSError):
            engine_timing = None
    mem = mem_block(measured["data"])
    return {
        "round": rnd,
        "rc": measured["rc"],
        "verdict": verdict,
        "error": error,
        "wall_ms": effective_wall,
        "process_wall_ms": measured["wall_ms"],
        "engine_timing": engine_timing,
        "mem": mem,
        "artifacts": str(run_dir),
        "stderr_tail": (measured["stderr_tail"] or "")[-1200:]
                       if (measured["rc"] not in (0, 1) or not measured["data"])
                       else None,
    }


def variant_record(task, kind, spec, runs):
    """Aggregate one variant's rounds into the suite record (unchanged shape)."""
    scenario_rel = spec["scenario"]
    scenario = SUITE_DIR / scenario_rel
    runs = sorted(runs, key=lambda r: r.get("round") or 0)
    walls = [r["wall_ms"] or 0 for r in runs]
    process_walls = [r["process_wall_ms"] or 0 for r in runs]
    peaks = [r["mem"]["peak_mib_all_cgroups"] for r in runs
             if r["mem"]["peak_mib_all_cgroups"] is not None]
    gibs = [r["mem"]["primary_anon_shmem_gib_min"] for r in runs
            if r["mem"]["primary_anon_shmem_gib_min"] is not None]
    first_bad = next((r for r in runs if r["verdict"] != "pass"), None)
    ordered = [r["verdict"] for r in runs]
    # Worst verdict wins: error/fail/unsupported/unknown over pass.
    rank = {"pass": 0, "fail": 1, "unsupported": 2, "error": 3}
    worst = max(ordered, key=lambda v: rank.get(v, 3))
    return {
        "variant": kind,
        "scenario": scenario_rel,
        "scenario_sha256": sha256(scenario),
        "expected": spec["expected"],
        "expected_error": spec.get("error"),
        "verdict": worst,
        "error": first_bad["error"] if first_bad else None,
        "task_ok": (worst == spec["expected"] and
                    (spec["expected"] != "fail" or
                     ((first_bad or {}).get("error") or {}).get("code") == spec.get("error"))),
        "false_pass": bool(kind == "wrong" and worst == "pass"),
        "wall_ms": int(sum(walls)),
        "wall_ms_p50": benchlib.stats(walls)["p50"] if walls else None,
        "process_wall_ms": int(sum(process_walls)),
        "process_wall_ms_p50": benchlib.stats(process_walls)["p50"] if process_walls else None,
        "runs": runs,
        "mem": {
            "metric": "anon+shmem (cgroup memory.stat, job+guest, sampler excluded)",
            "metric_type": "CGROUP_ANON_SHMEM",
            "primary_anon_shmem_gib_min": benchlib._round(sum(gibs)) if gibs else None,
            "cgroup_peak_mib": max(peaks) if peaks else None,
            "guest_scope_count": sum((r["mem"].get("guest_scope_count") or 0)
                                     for r in runs),
            "denominators": {f: [r["mem"][f] for r in runs] for f in (
                "primary_anon_shmem_gib_min", "resident_gib_min", "job_all_gib_min",
                "scope_gib_min_runner_formula", "infra_file_cache_gib_min",
                "sampler_overhead_gib_min")},
        },
    }


def run_variant(task, kind, spec, opts, env, stamp_s, inside_lock, acos_bin):
    """Sequential path (--concurrency 1): every round of one variant in turn."""
    runs = [run_round(task, kind, spec, opts, env, stamp_s, inside_lock, acos_bin, rnd)
            for rnd in range(1, opts["rounds"] + 1)]
    return variant_record(task, kind, spec, runs)

# --------------------------------------------------------------------------- #
# aggregation
# --------------------------------------------------------------------------- #
DENOMINATORS = (
    ("primary", "anon+shmem (job+guest, sampler excluded)", "primary_anon_shmem_gib_min",
     True, "CGROUP_ANON_SHMEM"),
    ("resident", "anon+shmem+kernel", "resident_gib_min", False, "CGROUP_ANON_SHMEM"),
    ("job_all_incl_file", "memory.current (includes shared file cache)",
     "job_all_gib_min", False, "CGROUP_MEMORY_CURRENT"),
    ("scope_runner_formula", "job scope only (pre-W45 runner formula)",
     "scope_gib_min_runner_formula", False, "CGROUP_MEMORY_CURRENT"),
)

def run_for(rec, kind, rnd):
    for r in (rec.get(kind) or {}).get("runs") or []:
        if r.get("round") == rnd:
            return r
    return None

def verified_map(active):
    out = {}
    for t in active:
        good = t.get("good") or {}
        out[t["id"]] = (good.get("verdict") == "pass")
    return out

def round_verified_map(active, rnd):
    """Per-round numerator: the good twin passed in THAT round."""
    out = {}
    for t in active:
        run = run_for(t, "good", rnd)
        out[t["id"]] = bool(run and run.get("verdict") == "pass")
    return out

def task_costs(active, field, rnd=None):
    """Per-task GiB.min (good + wrong twin). None if any needed value is missing."""
    costs = {}
    for t in active:
        total = 0.0
        ok = True
        kinds = ("good", "wrong")
        rounds = [rnd] if rnd is not None else \
            sorted({r.get("round") for k in kinds for r in ((t.get(k) or {}).get("runs") or [])})
        for kind in kinds:
            for rr in rounds:
                run = run_for(t, kind, rr)
                v = ((run or {}).get("mem") or {}).get(field)
                if v is None:
                    ok = False
                else:
                    total += v
        costs[t["id"]] = (total / len(rounds)) if (ok and rounds) else None
    return costs

def denominators_block(active, rounds, false_pass):
    out = {}
    for key, label, field, gate, metric_type in DENOMINATORS:
        per_round = []
        for rnd in range(1, rounds + 1):
            per_round.append(benchlib.m11_denominator(
                task_costs(active, field, rnd), round_verified_map(active, rnd),
                bar=LEGACY_BAR))
        head = benchlib.m11_denominator(task_costs(active, field), verified_map(active),
                                        bar=LEGACY_BAR)
        round_m11 = [x["m11_tasks_per_gib_min"] for x in per_round
                     if x["m11_tasks_per_gib_min"] is not None]
        if false_pass:
            head["m11_tasks_per_gib_min"] = 0.0
            head["passes_bar"] = False
            head["zeroed_by_false_pass"] = True
        head["per_round"] = per_round
        head["round_m11"] = round_m11
        head["m11_median"] = (benchlib.percentile(round_m11, 50)
                              if round_m11 else None)
        head["round_spread_pct"] = (
            round(100.0 * (max(round_m11) - min(round_m11)) / head["m11_tasks_per_gib_min"], 3)
            if len(round_m11) >= 2 and head["m11_tasks_per_gib_min"] else None)
        head["ci95_low_above_required"] = bool(
            head.get("ci95_low") is not None
            and head["ci95_low"] >= head["required_tasks_per_gib_min"])
        out[key] = {
            "name": label,
            "metric_type": metric_type,
            "gate_uses": gate,
            "m11": head,
            "note": ("PRIMARY: same type/state basis as the legacy 24.4 bar; the only "
                     "denominator the bar is evaluated on" if gate else
                     "recorded for continuity/debugging; NOT a gate value"),
        }
    return out

def scope_rows(task_records):
    rows = []
    for t in task_records:
        for kind in ("good", "wrong"):
            for run in (t.get(kind) or {}).get("runs") or []:
                mem = run.get("mem") or {}
                rows.append({
                    "variant": f"{t['id']}-{kind}-r{run.get('round')}",
                    "job_cgroup": mem.get("job_cgroup"),
                    "sampler_pid": (run.get("mem") or {}).get("sampler_pid"),
                })
    return rows

def main():
    opts = parse_args(sys.argv[1:])
    if not INDEX.exists():
        die(f"missing index {INDEX}")
    index = yaml.safe_load(INDEX.read_text())
    tasks = index.get("tasks", [])
    if opts["only"]:
        tasks = [t for t in tasks if t["id"] in opts["only"]]
        if not tasks:
            die("--only selected no task")
    acos_bin = opts["acos"] or os.environ.get("ACOS_SUITE_ACOS") or str(ROOT / "target/debug/acos")
    if not Path(acos_bin).exists():
        die(f"acos binary not found: {acos_bin} (build target/debug/acos; use "
            "tools/art-runtime/bench-release.sh for a release build)")
    for task in tasks:
        apk = ROOT / (task.get("apk") or "")
        if task.get("apk") and not apk.exists():
            die(f"{task['id']}: prebuilt fixture APK missing: {apk} (do not rebuild fixtures for the suite)")

    inside_lock = bool(os.environ.get("ACOS_ART_DATA"))
    if opts["concurrency"] > 1 and not inside_lock:
        die("--concurrency > 1 requires one suite-level ART lock: run under "
            "tools/art-runtime/run-exclusive.sh (it sets ACOS_ART_DATA)")
    gate = benchlib.host_gate(ROOT, check_lock=not inside_lock) if opts["host_gate"] else None
    contaminated = bool(gate and not gate["ok"])
    if contaminated:
        print("run-suite: WARNING host gate failed: " + "; ".join(gate["reasons"]),
              file=sys.stderr)

    env = dict(os.environ)
    acos_art = opts["acos_art"] or env.get("ACOS_ART_BIN") or str(ROOT / "target/debug/acos-art")
    if not Path(acos_art).exists():
        die(f"acos-art binary not found: {acos_art}")
    env["ACOS_ART_BIN"] = acos_art

    stamp_s = stamp()
    started = utc_now()
    print(f"run-suite: {len(tasks)} tasks, rounds={opts['rounds']}, "
          f"inside_lock={inside_lock}, fresh_scope_per_variant=guarded.sh, "
          f"sampler={opts['sampler']}, stamp={stamp_s}")
    task_records = []
    rec_by_id = {}
    active_tasks = []
    for task in tasks:
        rec = {
            "id": task["id"],
            "title": task.get("title"),
            "app": task.get("app"),
            "apk": task.get("apk"),
            "apk_sha256_declared": task.get("apk_sha256"),
            "apk_sha256_actual": None,
            "verbs": task.get("verbs") or [],
            "status": task.get("status", "ACTIVE"),
            "unsupported": task.get("unsupported") or [],
            "reason": task.get("reason"),
            "mapping": task.get("mapping"),
        }
        if task.get("apk"):
            rec["apk_sha256_actual"] = sha256(ROOT / task["apk"])
        task_records.append(rec)
        rec_by_id[task["id"]] = rec
        if task.get("status") == "UNSUPPORTED":
            rec.update({"verdict": "unsupported", "error": {"code": "UNSUPPORTED_EXPRESSION",
                        "message": task.get("reason") or "not expressible in the scenario format"},
                        "wall_ms": 0, "task_ok": True, "false_pass": False,
                        "good": None, "wrong": None})
            print(f"{task['id']:4s} UNSUPPORTED  {rec['error']['code']}")
            continue
        active_tasks.append(task)

    def good_spec(task):
        return {**task["good"], "expected": task["good"].get("expected", "pass"),
                "error": task["good"].get("error")}

    def finish_task(task, good, wrong):
        rec = rec_by_id[task["id"]]
        task_ok = bool(good["task_ok"] and wrong["task_ok"])
        rec.update({
            "verdict": "pass" if task_ok else "fail",
            "error": None if task_ok else ((good.get("error") or {}) if not good["task_ok"]
                                           else (wrong.get("error") or {})),
            "wall_ms": good["wall_ms"] + wrong["wall_ms"],
            "task_ok": task_ok,
            "false_pass": bool(good["false_pass"] or wrong["false_pass"]),
            "good": good,
            "wrong": wrong,
        })
        g_mem = good["mem"]["primary_anon_shmem_gib_min"]
        w_mem = wrong["mem"]["primary_anon_shmem_gib_min"]
        print(f"{task['id']:4s} good={good['verdict']:<11s} "
              f"wrong={wrong['verdict']:<11s} "
              f"code={(wrong.get('error') or {}).get('code')!s:<20s} "
              f"{'OK' if task_ok else 'MISMATCH'}  "
              f"{good['wall_ms'] + wrong['wall_ms']} ms  "
              f"mem={_fmt(g_mem)}+{_fmt(w_mem)} GiB.min  "
              f"guest_scopes={good['mem']['guest_scope_count']}/{wrong['mem']['guest_scope_count']}")

    host_rounds = []
    if opts["concurrency"] == 1:
        for task in active_tasks:
            good = run_variant(task, "good", good_spec(task), opts, env, stamp_s,
                               inside_lock, acos_bin)
            wrong = run_variant(task, "wrong", task["wrong"], opts, env, stamp_s,
                                inside_lock, acos_bin)
            finish_task(task, good, wrong)
    else:
        runs_by = {(t["id"], k): [] for t in active_tasks for k in ("good", "wrong")}
        for rnd in range(1, opts["rounds"] + 1):
            round_started = utc_now()
            round_t0 = time.time()
            monitor = benchlib.HostScopeMonitor(
                self_job=benchlib.cgroup_path(), interval=opts["interval"],
                discover_every=1,
                registry_dirs=[Path(opts["artifacts"]) / stamp_s / "_registry" / f"r{rnd}"])
            monitor.start()
            jobs = []
            for task in active_tasks:
                jobs.append((task, "good", good_spec(task)))
                jobs.append((task, "wrong", task["wrong"]))
            outcomes = []
            with ThreadPoolExecutor(max_workers=opts["concurrency"]) as pool:
                futures = {pool.submit(run_round, task, kind, spec, opts, env, stamp_s,
                                       inside_lock, acos_bin, rnd): (task, kind)
                           for task, kind, spec in jobs}
                for fut in as_completed(futures):
                    task, kind = futures[fut]
                    outcomes.append((task, kind, fut.result()))
            monitor.closing_sample()
            monitor.stop_event.set()
            monitor.join(timeout=5)
            round_finish = utc_now()
            round_wall = time.time() - round_t0
            for task, kind, run in outcomes:
                runs_by[(task["id"], kind)].append(run)
            per_run = sum(r["mem"]["primary_anon_shmem_gib_min"] or 0.0
                          for _, _, r in outcomes)
            overhead = sum(r["mem"]["sampler_overhead_gib_min"] or 0.0
                           for _, _, r in outcomes)
            host_raw = monitor.integral()
            agree = benchlib.scope_agreement(per_run, host_raw,
                                             sampler_overhead=overhead, tolerance=NOISE)
            verified_round = sum(1 for t, k, r in outcomes
                                 if k == "good" and r["verdict"] == "pass")
            seen_scopes = set()
            max_active = 0
            for s in monitor.samples:
                seen_scopes.update(s["cgroups"])
                max_active = max(max_active, len(s["cgroups"]))
            host_rounds.append({
                "round": rnd,
                "utc_start": round_started,
                "utc_finish": round_finish,
                "wall_s": round(round_wall, 3),
                "runs": len(outcomes),
                "concurrency": opts["concurrency"],
                "verified_good": verified_round,
                "scope_agreement": agree,
                "host_samples": len(monitor.samples),
                "host_discovered_scopes": len(seen_scopes),
                "host_max_active_scopes": max_active,
                "host_peak_mib": (round(monitor.peak() / benchlib.MIB, 3)
                                  if monitor.peak() is not None else None),
                "m11": (verified_round / per_run) if per_run else None,
            })
            print(f"round {rnd}: {len(outcomes)} runs at N={opts['concurrency']} in "
                  f"{round_wall:.1f}s, verified_good={verified_round}, "
                  f"per_run={_fmt(per_run)} host={_fmt(agree['host_gib_min'])} GiB.min "
                  f"(diff {agree['rel_diff_pct']}% vs {agree['tolerance'] * 100:.0f}% tol)")
        for task in active_tasks:
            good = variant_record(task, "good", good_spec(task),
                                  runs_by[(task["id"], "good")])
            wrong = variant_record(task, "wrong", task["wrong"],
                                   runs_by[(task["id"], "wrong")])
            finish_task(task, good, wrong)

    # ---- summary / gates ---------------------------------------------------
    active = [t for t in task_records if t["status"] != "UNSUPPORTED"]
    unsupported = [t for t in task_records if t["status"] == "UNSUPPORTED"]
    good_pass = sum(1 for t in active if (t.get("good") or {}).get("verdict") == "pass")
    good_fail = len(active) - good_pass
    wrong_expected = sum(1 for t in active
                         if (t.get("wrong") or {}).get("verdict") == "fail"
                         and ((t.get("wrong") or {}).get("error") or {}).get("code")
                         == (t.get("wrong") or {}).get("expected_error"))
    false_pass = sum(1 for t in task_records if t.get("false_pass"))
    false_fail_runs = []
    for t in active:
        g = t.get("good") or {}
        if g.get("verdict") == "pass":
            continue
        err = (g.get("error") or {})
        cls = benchlib.classify_error(err.get("code"), err.get("message")).get("class")
        if cls in ("harness", "infra_timeout", "host_guard") or g.get("verdict") is None:
            false_fail_runs.append({"task": t["id"], "code": err.get("code"), "class": cls})

    summary = {
        "tasks_total": len(task_records),
        "tasks_expressible": len(active),
        "tasks_unsupported": len(unsupported),
        "unsupported_ids": [t["id"] for t in unsupported],
        "verified_tasks": good_pass,
        "coverage": f"{good_pass}/{len(active)}" + (f" (+{len(unsupported)} UNSUPPORTED)" if unsupported else ""),
        "good_pass": good_pass,
        "good_fail": good_fail,
        "wrong_fail_expected": wrong_expected,
        "wrong_pass": false_pass,
        "false_pass_count": false_pass,
        "false_fail_count": len(false_fail_runs),
        "false_fail_detail": false_fail_runs,
        "task_ok_count": sum(1 for t in active if t.get("task_ok")),
        "contaminated_host_gate": contaminated,
    }

    scope_check = benchlib.check_variant_scopes(scope_rows(active))
    denoms = denominators_block(active, opts["rounds"], false_pass)
    primary = denoms["primary"]["m11"]
    infra_total = None
    infra_vals = [r["mem"]["infra_file_cache_gib_min"] for t in active
                  for k in ("good", "wrong") for r in (t.get(k) or {}).get("runs") or []
                  if r["mem"]["infra_file_cache_gib_min"] is not None]
    if infra_vals:
        infra_total = benchlib._round(sum(infra_vals) / max(1, opts["rounds"]))

    # W52 host-level cross-check: the alternative denominator over the same
    # concurrent windows, discovered independently of the per-run samplers.
    agreement_overall = None
    if host_rounds:
        per_run = sum(r["scope_agreement"]["per_run_sum_gib_min"] or 0.0
                      for r in host_rounds)
        host_raw = sum(r["scope_agreement"]["host_gib_min_raw"] or 0.0
                       for r in host_rounds)
        overhead = sum(r["scope_agreement"]["sampler_overhead_gib_min"] or 0.0
                       for r in host_rounds)
        agreement_overall = benchlib.scope_agreement(
            per_run, host_raw, sampler_overhead=overhead, tolerance=NOISE)
        agreement_overall["rounds"] = len(host_rounds)
        agreement_overall["all_rounds_within_noise"] = all(
            r["scope_agreement"]["within_noise"] for r in host_rounds)
        agreement_overall["ok"] = bool(agreement_overall["within_noise"]
                                       and agreement_overall["all_rounds_within_noise"])
        agreement_overall["per_round"] = [r["scope_agreement"] for r in host_rounds]

    spread_ok = (primary.get("round_spread_pct") is None
                 or primary["round_spread_pct"] <= 15.0)
    agreement_ok = agreement_overall is None or bool(agreement_overall["ok"])
    gate_eligible = bool(opts["concurrency"] == 10 and opts["rounds"] >= 3
                         and primary["m11_tasks_per_gib_min"] is not None
                         and scope_check["ok"] and not contaminated
                         and false_pass == 0 and agreement_ok and spread_ok)
    gate_reasons = []
    if opts["concurrency"] != 10:
        gate_reasons.append("N=%d (gate requires N=10)" % opts["concurrency"])
    if opts["rounds"] < 3:
        gate_reasons.append("rounds=%d (gate requires 3)" % opts["rounds"])
    if contaminated:
        gate_reasons.append("host gate failed")
    if not scope_check["ok"]:
        gate_reasons.append("scope check failed")
    if false_pass:
        gate_reasons.append("false pass > 0")
    if not agreement_ok:
        gate_reasons.append("host-level/per-run integrals disagree beyond noise")
    if not spread_ok:
        gate_reasons.append("round spread > 15 %")

    verdict = "INVALID_SCOPE"
    if scope_check["ok"] and primary["m11_tasks_per_gib_min"] is not None:
        if false_pass:
            verdict = "FAIL"
        elif not spread_ok:
            verdict = "NOISY"
        elif not agreement_ok:
            verdict = "SCOPE_MISMATCH"
        elif primary.get("ci95_low_above_required"):
            verdict = "PASS"
        else:
            verdict = "FAIL"
    m11_block = {
        "formula": "M11 = verified_tasks / sum(primary anon+shmem GiB.min); primary = "
                   "job cgroup + every guest cgroup memory.stat (anon+shmem), 100 ms "
                   "samples with a closing sample, sampler cost subtracted; if "
                   "false_pass > 0 then M11 := 0",
        "primary_denominator": "anon+shmem (CGROUP_ANON_SHMEM)",
        "legacy_derived_bar_tasks_per_gib_min": LEGACY_BAR,
        "legacy_derivation": "((206 MiB / 10) + 112.1 MiB) / 1024 * (19 s / 60) "
                             "= 0.0410 GiB*min/task => 24.4 tasks/GiB*min",
        "legacy_bar_inputs": {
            "core": "206.0 MiB, legacy W1 diet bare core, cgroup anon+shmem, settled 60 s",
            "marginal": "112.1 MiB, legacy N=5->10 marginal, cgroup memory.current, warm",
            "wall": "19 s, legacy U2 dev-loop wall (build->install->...), warm/install-bound",
            "types_mixed": True,
            "comparable_with_anon_shmem_primary": False,
        },
        "noise": NOISE,
        "required_tasks_per_gib_min": round(LEGACY_BAR * (1 + NOISE), 4),
        "verified_tasks": good_pass,
        "false_pass_count": false_pass,
        "false_pass_zeroes_m11": bool(false_pass),
        "m11_tasks_per_gib_min": primary["m11_tasks_per_gib_min"],
        "m11_median": primary.get("m11_median"),
        "gib_min": primary["gib_min"],
        "ci95_low": primary["ci95_low"],
        "ci95_high": primary["ci95_high"],
        "ci95_width_pct": primary["ci95_width_pct"],
        "ci95_low_above_required": primary.get("ci95_low_above_required"),
        "rounds": opts["rounds"],
        "round_m11": primary["round_m11"],
        "round_spread_pct": primary["round_spread_pct"],
        "round_spread_ok": spread_ok,
        "above_legacy_bar": primary["passes_bar"],
        "verdict_vs_bar": verdict,
        "verdict_rule": "PASS only if the CI95 low >= bar*(1+noise), the round spread "
                        "<= 15 %, the scope check passes, false_pass is 0 and the "
                        "host-level/per-run integrals agree within noise",
        "denominators": denoms,
        "host_level_alternative": agreement_overall,
        "n_concurrent": opts["concurrency"],
        "gate_eligible": gate_eligible,
        "gate_eligible_reason": ("; ".join(gate_reasons) if gate_reasons else
                                 "N=10 x 3 rounds, release, scope check ok, no false "
                                 "pass, host cross-check within noise"),
        "infra_file_cache_gib_min": infra_total,
        "infra_file_cache_note": "shared read-only page cache (file - shmem) across the "
                                 "same runs; reported separately and never charged to a task",
        "scope_check": scope_check,
        "note": "Only the primary anon+shmem number is compared with the 24.4 bar. "
                "resident/job_all/scope_runner_formula are reported with gate_uses=false.",
    }
    provenance = benchlib.provenance(
        ROOT, binaries=[acos_bin, acos_art], profile=None, gate=gate, engine="art",
        extra={"suite": index.get("suite"), "stamp": stamp_s,
               "n_concurrent": opts["concurrency"]})
    result = {
        "schema": "acos-suite/2",
        "suite": index.get("suite"),
        "stamp": stamp_s,
        "utc_start": started,
        "utc_finish": utc_now(),
        "engine": "art",
        "n_concurrent": opts["concurrency"],
        "rounds": opts["rounds"],
        "concurrent_rounds": host_rounds,
        "inside_exclusive_lock": inside_lock,
        "host_gate": gate,
        "provenance": provenance,
        "measurement": {
            "fresh_scope_per_variant": True,
            "scope_wrapper": "tools/guarded.sh per variant run",
            "sampler": opts["sampler"],
            "interval_s": opts["interval"],
            "guest_cgroups": "discovered from the launcher's <run>/cgroup evidence file "
                             "and a ppid-chain descendant scan (sibling scope included)",
            "closing_sample": True,
            "sampler_excluded": True,
            "primary_denominator": "anon+shmem",
            "infra_file_cache_charged": False,
            "artifacts_kept": bool(opts["keep_artifacts"]),
            "concurrency": opts["concurrency"],
            "concurrent_scopes": "one guarded scope per run, N runs at once",
            "host_level_cross_check": ("independent union of all job+guest cgroups over "
                                       "each concurrent window" if host_rounds else None),
        },
        "tasks": task_records,
        "summary": summary,
        "m11": m11_block,
        "artifacts": {"root": str(Path(opts["artifacts"]) / stamp_s),
                      "pruned": False, "runs": []},
    }
    out = Path(opts["out"]) if opts["out"] else (BENCH_DIR / f"suite-{stamp_s}.json")
    out.parent.mkdir(parents=True, exist_ok=True)
    benchlib.write_json(out, result)

    # ---- artifact pruning (after the summary JSON exists: decision e) ------
    prune_runs = []
    for t in task_records:
        for kind in ("good", "wrong"):
            for run in (t.get(kind) or {}).get("runs") or []:
                art = run.get("artifacts")
                if not art:
                    continue
                res = benchlib.prune_artifacts(
                    art, keep=opts["keep_artifacts"],
                    passed=(run.get("verdict") == "pass"))
                prune_runs.append({
                    "variant": f"{t['id']}-{kind}-r{run.get('round')}",
                    "dir": res["dir"], "pruned": res["pruned"],
                    "deleted": res["deleted"], "kept": res["kept"],
                    "freed_bytes": res["freed_bytes"], "kept_bytes": res["kept_bytes"],
                    "reason": res["reason"]})
    result["artifacts"] = {
        "root": str(Path(opts["artifacts"]) / stamp_s),
        "kept_all": bool(opts["keep_artifacts"]),
        "pruned": any(r["pruned"] for r in prune_runs),
        "freed_bytes": sum(r["freed_bytes"] for r in prune_runs),
        "kept_bytes": sum(r["kept_bytes"] for r in prune_runs),
        "runs": prune_runs,
    }
    benchlib.write_json(out, result)

    print(f"\nverified_tasks={summary['verified_tasks']}/{summary['tasks_expressible']} "
          f"unsupported={summary['tasks_unsupported']} "
          f"false_pass={summary['false_pass_count']} "
          f"false_fail={summary['false_fail_count']}")
    print(f"M11(primary anon+shmem)={_fmt(primary['m11_tasks_per_gib_min'])} tasks/GiB.min "
          f"gib_min={_fmt(primary['gib_min'])} "
          f"ci95={_fmt(primary['ci95_low'])}..{_fmt(primary['ci95_high'])} "
          f"bar={LEGACY_BAR} verdict={verdict} gate_eligible={gate_eligible}")
    print(f"  n_concurrent={opts['concurrency']} rounds={opts['rounds']} "
          f"round_m11={primary['round_m11']} spread={primary['round_spread_pct']}% "
          f"ci_low_above_required={primary.get('ci95_low_above_required')}")
    if agreement_overall is not None:
        print(f"  host-level cross-check: per_run={_fmt(agreement_overall['per_run_sum_gib_min'])} "
              f"host={_fmt(agreement_overall['host_gib_min'])} GiB.min "
              f"diff={agreement_overall['rel_diff_pct']}% "
              f"tol={agreement_overall['tolerance'] * 100:.0f}% "
              f"agree={agreement_overall['within_noise']} "
              f"all_rounds={agreement_overall['all_rounds_within_noise']}")
    for key, blk in denoms.items():
        mm = blk["m11"]
        print(f"  {key:22s} {blk['name']:48s} gib_min={_fmt(mm['gib_min'])} "
              f"m11={_fmt(mm['m11_tasks_per_gib_min'])} gate={blk['gate_uses']}")
    print(f"  infra_file_cache (not charged) = {_fmt(infra_total)} GiB.min")
    print(f"  fresh-scope check: ok={scope_check['ok']} "
          f"distinct_scopes={scope_check['distinct_scopes']} "
          f"variants={scope_check['variants']}")
    if scope_check["errors"]:
        for e in scope_check["errors"]:
            print(f"  SCOPE DEFECT: {e}", file=sys.stderr)
    print(f"  artifacts: pruned={result['artifacts']['pruned']} "
          f"freed={result['artifacts']['freed_bytes'] / (1 << 20):.1f} MiB "
          f"kept={result['artifacts']['kept_bytes'] / (1 << 20):.1f} MiB")
    print(f"wrote {out}")
    if (false_pass > 0 or good_fail > 0 or summary["task_ok_count"] != len(active)
            or not scope_check["ok"] or not agreement_ok):
        print("run-suite: NOT ACCEPTED (see tasks[], m11.scope_check, "
              "m11.host_level_alternative)", file=sys.stderr)
        raise SystemExit(1)
    print("run-suite: ACCEPTED (all expressible correct variants pass; all wrong variants "
          "fail with the declared code; one fresh scope per variant; host-level integral "
          "agrees with the per-run sum)")
    raise SystemExit(0)

def _fmt(v):
    if v is None:
        return "none"
    return ("%.5f" % v) if abs(v) < 10 else ("%.2f" % v)

main()
PY
