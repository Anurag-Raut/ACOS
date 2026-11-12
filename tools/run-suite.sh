#!/usr/bin/env bash
# run-suite.sh -- run the frozen agent-task suite T01-T12 (scenarios/suite/) on
# the real ART engine, one variant at a time, and write
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
# M11 formula (advisor item 3, exact):
#   job_gib_min = sum over 100 ms samples of memory.current_bytes / 2^30 * dt_s / 60
#                 per job cgroup, from job submit to cleanup complete
#   M11         = verified_good / sum(job_gib_min) over the suite at N=10
#                 concurrent, 3 rounds, median
#   if false_pass > 0: M11 := 0
#   legacy-derived bar:
#     ((206 MiB core / 10) + 112.1 MiB marginal) / 1024 * (19 s / 60)
#       = 0.0410 GiB*min/task  =>  24.4 tasks/GiB*min (legacy-derived-bound)
#     ACOS must exceed 24.4 * (1 + noise).
# This runner executes N=1, one round by default, so its m11 value is a
# placeholder for cost visibility (gate_eligible=false); the gateable N=10,
# 3-round number is produced by the bench-session suite (advisor items 1/3).
# When the runner is inside a guarded systemd scope (memory.max finite, which is
# how W30 runs it) it samples that cgroup every 100 ms and fills job_gib_min;
# otherwise job_gib_min/m11 stay null rather than being estimated.
#
# Usage (mandated W30 form):
#   tools/art-runtime/run-exclusive.sh --task W30 -- \
#     tools/guarded.sh --mem 8G --time 900 -- tools/run-suite.sh
#
# Standalone use (the script then wraps every variant in
# run-exclusive.sh + guarded.sh itself, still one ART job at a time):
#   tools/run-suite.sh [--rounds N] [--only T01,T02] [--acos PATH]
#
# Exit: 0 = all expressible variants behaved as declared (UNSUPPORTED recorded);
#       1 = false pass, wrong-variant mismatch, or a correct variant failed;
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
import re
import subprocess
import sys
import time
from pathlib import Path

import yaml

ROOT = Path(os.environ["ACOS_SUITE_ROOT"])
sys.path.insert(0, str(ROOT / "tools"))
import benchlib  # noqa: E402  (W29; host gate, provenance, stats, error classes)

SUITE_DIR = ROOT / "scenarios/suite"
INDEX = SUITE_DIR / "INDEX.yaml"
BENCH_DIR = ROOT / "results/bench"
ARTIFACT_DIR = ROOT / ".cache/w30-suite"

def utc_now():
    return datetime.datetime.now(datetime.timezone.utc).isoformat(timespec="seconds")

def stamp():
    return datetime.datetime.now(datetime.timezone.utc).strftime("%Y%m%dT%H%M%SZ")

def die(message, code=2):
    print(f"run-suite: {message}", file=sys.stderr)
    raise SystemExit(code)

def parse_args(argv):
    opts = {"rounds": 1, "only": None, "acos": None, "acos_art": None,
            "out": None, "host_gate": True, "timeout": 180}
    i = 0
    while i < len(argv):
        a = argv[i]
        if a == "--rounds":
            i += 1; opts["rounds"] = int(argv[i])
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
        elif a == "--no-host-gate":
            opts["host_gate"] = False
        elif a in ("-h", "--help"):
            print("run-suite.sh [--rounds N] [--only T01,T02] [--acos PATH] "
                  "[--acos-art PATH] [--out FILE] [--timeout S] [--no-host-gate]")
            raise SystemExit(0)
        else:
            die(f"unknown argument {a!r}")
        i += 1
    return opts

def sha256(path):
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()

# --------------------------------------------------------------------------- #
# cgroup sampling (per-job memory.current every 100 ms)
# --------------------------------------------------------------------------- #
def cgroup_path():
    try:
        text = Path("/proc/self/cgroup").read_text()
    except OSError:
        return None
    for line in text.splitlines():
        if line.startswith("0::"):
            rel = line[3:].strip()
            if not rel:
                return None
            return Path("/sys/fs/cgroup") / rel.lstrip("/")
    return None

def cgroup_sampler():
    """Return (path, None) when this process is in a finite-memory leaf cgroup."""
    path = cgroup_path()
    if not path:
        return None
    try:
        mx = (path / "memory.max").read_text().strip()
        if not (path / "memory.current").exists() or mx == "max":
            return None
    except OSError:
        return None
    return path

def read_cgroup(path, name):
    try:
        return int((path / name).read_text().strip())
    except (OSError, ValueError):
        return None

def run_one(cmd, env, cgpath, timeout_s):
    """Run one verify command, sampling memory.current. Returns dict."""
    t0 = time.time()
    t0_wall = time.monotonic()
    proc = subprocess.Popen(cmd, cwd=str(ROOT), env=env,
                            stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
    samples = []
    peak = None
    while True:
        if cgpath is not None:
            now = time.time()
            cur = read_cgroup(cgpath, "memory.current")
            if cur is not None:
                samples.append({"t": now, "bytes": cur})
                peak = cur if peak is None else max(peak, cur)
        if proc.poll() is not None:
            break
        if time.monotonic() - t0_wall > timeout_s + 10:
            proc.kill()
            break
        time.sleep(0.1)
    out, err = proc.communicate()
    t1 = time.time()
    wall_ms = int((t1 - t0) * 1000)
    gib_min = benchlib.sample_integral(samples) if samples else None
    return {
        "rc": proc.returncode,
        "stdout": out,
        "stderr": err,
        "wall_ms": wall_ms,
        "cgroup_samples": len(samples),
        "cgroup_peak_mib": round(peak / (1 << 20), 3) if peak is not None else None,
        "cgroup_gib_min": round(gib_min, 9) if gib_min is not None else None,
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

def run_variant(task, kind, spec, opts, env, stamp_s, cgpath, inside_lock,
                acos_bin):
    scenario_rel = spec["scenario"]
    scenario = SUITE_DIR / scenario_rel
    if not scenario.exists():
        die(f"{task['id']}-{kind}: missing scenario {scenario}")
    artifacts = ARTIFACT_DIR / stamp_s / f"{task['id']}-{kind}"
    artifacts.mkdir(parents=True, exist_ok=True)
    acos_art = opts["acos_art"] or env.get("ACOS_ART_BIN") or str(ROOT / "target/debug/acos-art")
    env = dict(env)
    env["ACOS_ART_BIN"] = acos_art
    runs = []
    for rnd in range(1, opts["rounds"] + 1):
        cmd = [acos_bin, "verify", str(scenario), "--engine", "art", "--json",
               "--artifacts", str(artifacts / f"r{rnd}")]
        if not inside_lock:
            cmd = [str(ROOT / "tools/art-runtime/run-exclusive.sh"),
                   "--task", f"W30-{task['id']}-{kind}",
                   "--", str(ROOT / "tools/guarded.sh"),
                   "--mem", "8G", "--time", str(opts["timeout"]), "--", *cmd]
        run = run_one(cmd, env, cgpath, opts["timeout"])
        envelope = {}
        try:
            envelope = json.loads(run["stdout"])
        except json.JSONDecodeError:
            pass
        verdict_file = artifacts / f"r{rnd}" / "verdict.json"
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
        code = (error or {}).get("code")
        effective_wall = (verdict_obj.get("resources") or {}).get("wall_ms", run["wall_ms"])
        runs.append({
            "round": rnd,
            "rc": run["rc"],
            "verdict": verdict,
            "error": error,
            "wall_ms": effective_wall,
            "cgroup_peak_mib": run["cgroup_peak_mib"],
            "cgroup_gib_min": run["cgroup_gib_min"],
            "artifacts": str(artifacts / f"r{rnd}"),
            "stderr_tail": (run["stderr"] or "")[-1200:] if run["rc"] not in (0, 1) else None,
        })
    walls = [r["wall_ms"] or 0 for r in runs]
    peaks = [r["cgroup_peak_mib"] for r in runs if r["cgroup_peak_mib"] is not None]
    gibs = [r["cgroup_gib_min"] for r in runs if r["cgroup_gib_min"] is not None]
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
        "runs": runs,
        "mem": {
            "metric": "cgroup memory.current (sampled 100 ms)" if peaks else None,
            "cgroup_peak_mib": max(peaks) if peaks else None,
            "cgroup_gib_min": round(sum(gibs), 9) if gibs else None,
        },
    }

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
        die(f"acos binary not found: {acos_bin} (build target/debug/acos; no release requirement for correctness)")
    for task in tasks:
        apk = ROOT / (task.get("apk") or "")
        if task.get("apk") and not apk.exists():
            die(f"{task['id']}: prebuilt fixture APK missing: {apk} (do not rebuild fixtures for the suite)")

    inside_lock = bool(os.environ.get("ACOS_ART_DATA"))
    cgpath = cgroup_sampler()
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
          f"inside_lock={inside_lock}, cgroup={cgpath or 'none'}, stamp={stamp_s}")
    task_records = []
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
        if task.get("status") == "UNSUPPORTED":
            rec.update({"verdict": "unsupported", "error": {"code": "UNSUPPORTED_EXPRESSION",
                        "message": task.get("reason") or "not expressible in the scenario format"},
                        "wall_ms": 0, "task_ok": True, "false_pass": False, "good": None, "wrong": None})
            task_records.append(rec)
            print(f"{task['id']:4s} UNSUPPORTED  {rec['error']['code']}")
            continue
        good = run_variant(task, "good", {**task["good"], "expected": task["good"].get("expected", "pass"),
                                         "error": task["good"].get("error")},
                           opts, env, stamp_s, cgpath, inside_lock, acos_bin)
        wrong = run_variant(task, "wrong", task["wrong"], opts, env, stamp_s, cgpath,
                            inside_lock, acos_bin)
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
        task_records.append(rec)
        print(f"{task['id']:4s} good={good['verdict']:<11s} "
              f"wrong={wrong['verdict']:<11s} "
              f"code={(wrong.get('error') or {}).get('code')!s:<20s} "
              f"{'OK' if task_ok else 'MISMATCH'}  "
              f"{good['wall_ms'] + wrong['wall_ms']} ms")

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
    def task_gib(t):
        return sum((t.get(k) or {}).get("mem", {}).get("cgroup_gib_min") or 0.0
                   for k in ("good", "wrong"))

    def task_has_gib(t):
        return any((t.get(k) or {}).get("mem", {}).get("cgroup_gib_min") is not None
                   for k in ("good", "wrong"))

    gib_total = sum(task_gib(t) for t in active)
    has_gib = any(task_has_gib(t) for t in active)
    legacy_bar = 24.4
    m11 = None
    if has_gib and gib_total > 0:
        m11 = 0.0 if false_pass else good_pass / gib_total
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
    m11_block = {
        "formula": "M11 = verified_good / sum(job_gib_min); job_gib_min = "
                   "integral(memory.current_bytes/2^30 dt_s/60) per job cgroup; "
                   "if false_pass > 0 then M11 := 0",
        "legacy_derived_bar_tasks_per_gib_min": legacy_bar,
        "legacy_derivation": "((206 MiB / 10) + 112.1 MiB) / 1024 * (19 s / 60) "
                             "= 0.0410 GiB*min/task => 24.4 tasks/GiB*min",
        "acos_bar_with_noise": round(legacy_bar * 1.05, 3),
        "verified_tasks": good_pass,
        "job_gib_min": round(gib_total, 6) if has_gib else None,
        "m11_tasks_per_gib_min": round(m11, 4) if m11 is not None else None,
        "m11_above_legacy_bar": (m11 > legacy_bar * 1.05) if m11 is not None else None,
        "false_pass_zeroes_m11": bool(false_pass),
        "n_concurrent": 1,
        "rounds": opts["rounds"],
        "gate_eligible": False,
        "gate_eligible_reason": "W30 correctness run: N=1, rounds=" + str(opts["rounds"]) +
                                " (the gateable N=10 x 3-round number comes from the "
                                "bench-session suite, advisor items 1/3)",
        "cgroup_source": str(cgpath) if cgpath else None,
        "note": ("job_gib_min is filled from per-job cgroup memory.current samples when the "
                 "runner is inside a finite-memory guarded scope; otherwise it stays null "
                 "rather than being estimated."),
    }
    provenance = benchlib.provenance(
        ROOT, binaries=[acos_bin, acos_art], profile=None, gate=gate, engine="art",
        extra={"suite": index.get("suite"), "stamp": stamp_s})
    result = {
        "schema": "acos-suite/1",
        "suite": index.get("suite"),
        "stamp": stamp_s,
        "utc_start": started,
        "utc_finish": utc_now(),
        "engine": "art",
        "n_concurrent": 1,
        "rounds": opts["rounds"],
        "inside_exclusive_lock": inside_lock,
        "host_gate": gate,
        "provenance": provenance,
        "tasks": task_records,
        "summary": summary,
        "m11": m11_block,
    }
    out = Path(opts["out"]) if opts["out"] else (BENCH_DIR / f"suite-{stamp_s}.json")
    out.parent.mkdir(parents=True, exist_ok=True)
    benchlib.write_json(out, result)
    print(f"\nverified_tasks={summary['verified_tasks']}/{summary['tasks_expressible']} "
          f"unsupported={summary['tasks_unsupported']} "
          f"false_pass={summary['false_pass_count']} "
          f"false_fail={summary['false_fail_count']} "
          f"m11={m11_block['m11_tasks_per_gib_min']} "
          f"job_gib_min={m11_block['job_gib_min']}")
    print(f"wrote {out}")
    if false_pass > 0 or good_fail > 0 or summary["task_ok_count"] != len(active):
        print("run-suite: NOT ACCEPTED (see tasks[] for the mismatching variant)", file=sys.stderr)
        raise SystemExit(1)
    print("run-suite: ACCEPTED (all expressible correct variants pass; all wrong variants fail with the declared code)")
    raise SystemExit(0)

main()
PY
