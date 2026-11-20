import datetime
import glob
import json
import os
import re
import shutil
import statistics
import subprocess
import sys
import time
import uuid

(ROOT, OUT, PROFILE, ACOS, ACOS_ART, ENGINE, REPS, SAMPLE_MS, SETTLE_S,
 TIMEOUT_S, ARTIFACTS, APK_OVERRIDE, CONTAMINATED) = sys.argv[1:14]
SCENARIOS = sys.argv[14:]
REPS, SAMPLE_MS, SETTLE_S, TIMEOUT_S = int(REPS), int(SAMPLE_MS), int(SETTLE_S), int(TIMEOUT_S)
sys.path.insert(0, os.path.join(ROOT, "tools"))
import benchlib  # noqa: E402

# A run on a host that failed the gate is a CORRECTNESS run, never a timing one.
VERDICT_ELIGIBLE = PROFILE == "release" and CONTAMINATED != "1"
MIB = 1024.0 * 1024.0
GIB = 1024.0 ** 3


def utc():
    return datetime.datetime.now(datetime.timezone.utc).isoformat(timespec="seconds")


import shutil


def sha256(path):
    return benchlib.sha256_file(os.path.join(ROOT, path)) if path and not os.path.isabs(path) \
        else benchlib.sha256_file(path)


def read_scenario(path):
    """Minimal first-level YAML field reader (no PyYAML dependency).

    Besides name/app it reads the honest declaration this runner has to obey:

    * ``expectation`` (pass | fail | non_pass | unsupported),
    * ``expect_error`` (the code/reason a negative pins), and
    * ``needs_env`` (host capabilities the run needs; skipped when absent).
    """
    with open(os.path.join(ROOT, path), errors="replace") as f:
        text = f.read()
    def field(name):
        m = re.search(r"(?m)^%s:\s*(.+?)\s*$" % name, text)
        return m.group(1).strip().strip('"\'') if m else None
    expectation = (field("expectation") or "pass").lower()
    name = field("name") or os.path.splitext(os.path.basename(path))[0]
    app = field("app")
    # `expect_error: {code: TIMEOUT, reason: x}` or a block form with one key
    # per line. A negative that pins its failure must be counted correct only
    # when the run produced exactly that code/reason.
    expect_error = None
    m = re.search(r"(?m)^expect_error:\s*\{([^}]*)\}\s*$", text)
    if m:
        expect_error = {}
        for part in m.group(1).split(","):
            if ":" in part:
                key, value = part.split(":", 1)
                expect_error[key.strip()] = value.strip().strip('"\'')
    else:
        m = re.search(r"(?ms)^expect_error:\s*\n((?:[ \t]+\w+:.*\n?)+)", text)
        if m:
            expect_error = {}
            for line in m.group(1).splitlines():
                if ":" in line:
                    key, value = line.split(":", 1)
                    expect_error[key.strip()] = value.strip().strip('"\'')
    needs = []
    m = re.search(r"(?m)^needs_env:\s*\[(.*?)\]\s*$", text)
    if m:
        needs = [w.strip().strip('"\'') for w in m.group(1).split(",") if w.strip()]
    norm = path.replace(os.sep, "/")
    if "/negative/" in norm or expectation in ("fail", "non_pass", "unsupported"):
        variant = "negative"
    else:
        variant = "good"
    return {"name": name, "app": app, "expectation": expectation,
            "expected": "pass" if expectation == "pass" else "fail",
            "expect_error": expect_error, "needs_env": needs,
            "variant": variant, "sha256": sha256(path)}


# Host capabilities a scenario may name in `needs_env` (acos-protocol's
# KNOWN_NEEDS_ENV). Present == the scenario may run.
def needs_env_present(name):
    if name == "pasta":
        return bool(os.environ.get("ACOS_SPIKE_NET_PASTA") or shutil.which("pasta"))
    return False


def declared_failure_matches(row, meta):
    """The run's error matches the code/reason the scenario pins."""
    expected = meta.get("expect_error")
    if not expected:
        return None                      # nothing pinned
    err = row.get("error") or {}
    if (err.get("code") or "") != (expected.get("code") or ""):
        return False
    reason = expected.get("reason")
    if reason and (err.get("reason") or "") != reason:
        return False
    return True


def negative_is_correct(row, meta):
    """A negative is correct only when it did not pass AND matches its pin.

    "It went red somehow" is not proof: a harness error (INTERNAL, GUEST_EXITED
    without a declaration, an infrastructure timeout) on a negative proves
    nothing about the scenario. A pinned `expect_error` is authoritative; an
    unpinned `fail` accepts a real non-pass (assertion/app_crash/unsupported or
    the scenario's own step deadline), never a harness or host-guard failure.
    """
    if row.get("verdict") == "pass":
        return False
    pinned = declared_failure_matches(row, meta)
    if pinned is not None:
        return pinned
    cls = (row.get("error") or {}).get("class")
    return cls in ("assertion", "app_crash", "unsupported", "step_timeout")


def detect_engine_flag():
    try:
        help_text = subprocess.run([ACOS, "verify", "--help"], capture_output=True,
                                   text=True, timeout=60).stdout
    except Exception:
        help_text = ""
    return "--engine" in help_text


ENGINE_FLAG = detect_engine_flag() if ENGINE in ("auto", "art") else False
if ENGINE == "art" and not ENGINE_FLAG:
    print("run-matrix: REFUSING: --engine art requested but this binary has no "
          "--engine flag (build a release binary with the ART engine)", file=sys.stderr)
    sys.exit(6)
ENGINE_LABEL = ("art" if (ENGINE_FLAG and ENGINE in ("auto", "art")) else "default")


def cg_read(path, name):
    try:
        with open(os.path.join(path, name)) as f:
            return f.read().strip()
    except OSError:
        return None


def cg_read_map(path, name, keys):
    raw = cg_read(path, name)
    out = {}
    if not raw:
        return out
    for line in raw.splitlines():
        parts = line.split()
        if len(parts) == 2 and parts[0] in keys:
            try:
                out[parts[0]] = int(parts[1])
            except ValueError:
                pass
    return out


def find_scope(unit, deadline):
    patterns = [
        "/sys/fs/cgroup/user.slice/user-*/user@*/app.slice/%s.scope" % unit,
        "/sys/fs/cgroup/user.slice/user-*.slice/%s.scope" % unit,
        "/sys/fs/cgroup/system.slice/%s.scope" % unit,
    ]
    while time.time() < deadline:
        for pat in patterns:
            hits = glob.glob(pat)
            if hits:
                return hits[0]
        time.sleep(0.02)
    return None


def pss_mib_of(cg_path):
    """Secondary account: sum PSS over every pid in the run's own cgroup."""
    procs = cg_read(cg_path, "cgroup.procs")
    if not procs:
        return None
    total_kib = 0
    for pid in procs.split():
        try:
            with open("/proc/%s/smaps_rollup" % pid) as f:
                for line in f:
                    if line.startswith("Pss:"):
                        total_kib += int(line.split()[1])
                        break
        except (OSError, ValueError):
            continue
    return total_kib / 1024.0


def meminfo_mib():
    try:
        return benchlib.read_meminfo()["mem_available_mib"]
    except OSError:
        return None


def loadavg1():
    try:
        return benchlib.read_loadavg()["loadavg1"]
    except OSError:
        return None


def stop_scope(unit):
    for cmd in (["systemctl", "--user", "kill", "--signal=KILL", unit + ".scope"],
                ["systemctl", "--user", "reset-failed", unit + ".scope"]):
        try:
            subprocess.run(cmd, capture_output=True, text=True, timeout=20)
        except Exception:
            pass


def first_frame_from_steps(steps):
    """Cumulative step ms through the first step that produced a frame."""
    total = 0.0
    for st in steps or []:
        d = st.get("duration_ms")
        total += float(d) if d is not None else 0.0
        if st.get("frame_seq") is not None or st.get("step") == "capture":
            return round(total, 2)
    return None


def run_one(scenario, rep, index):
    meta = read_scenario(scenario)
    rid = "%02d-%s-r%d" % (index, meta["name"].replace("/", "-"), rep)
    unit = "acos-bench-%s" % uuid.uuid4().hex[:12]
    adir = os.path.join(ARTIFACTS, rid)
    # Do NOT create `adir` here. `acos verify --artifacts` owns its artifacts
    # directory (ADR-0026, "one run owns one artifacts directory"): a directory
    # that already holds files is another run's evidence and is refused with
    # INVALID_ARGUMENT, so creating it and writing our own stdout/stderr into it
    # before the run made every row `verdict=error class=harness` (REL3P).
    # The harness's own logs live in a driver scratch dir and move into `adir`
    # after verify has finished writing its evidence.
    logdir = os.path.join(ARTIFACTS, ".driver")
    os.makedirs(logdir, exist_ok=True)
    stdout_path = os.path.join(logdir, rid + ".stdout.json")
    stderr_path = os.path.join(logdir, rid + ".stderr.txt")
    cmd = [ACOS, "verify", scenario, "--json", "--artifacts", adir]
    if APK_OVERRIDE:
        cmd += ["--apk", APK_OVERRIDE]
    if ENGINE_FLAG:
        cmd += ["--engine", "art"]
    wrapped = ["systemd-run", "--user", "--scope", "--quiet", "--collect",
               "--unit", unit, "--property=MemoryMax=6G"] + cmd

    host_start = {"loadavg1": loadavg1(), "mem_available_mib": meminfo_mib()}
    samples = []
    cg_path = None
    cg_source = "unit-scope"
    t0 = time.perf_counter()
    deadline_scope = time.time() + 15
    timed_out = False
    with open(stdout_path, "w") as out_f, open(stderr_path, "w") as err_f:
        proc = subprocess.Popen(wrapped, stdout=out_f, stderr=err_f,
                                start_new_session=True)
        while proc.poll() is None:
            now = time.time()
            if cg_path is None:
                cg_path = find_scope(unit, deadline_scope)
            if cg_path:
                cur = cg_read(cg_path, "memory.current")
                stat = cg_read_map(cg_path, "memory.stat", ("anon", "shmem"))
                samples.append({
                    "t": now,
                    "bytes": int(cur) if cur and cur.isdigit() else None,
                    "anon_shmem_bytes": (stat.get("anon", 0) + stat.get("shmem", 0))
                                        if stat else None,
                    "pss_mib": pss_mib_of(cg_path),
                })
            if time.perf_counter() - t0 > TIMEOUT_S:
                timed_out = True
                proc.terminate()
                break
            time.sleep(SAMPLE_MS / 1000.0)
        try:
            rc = proc.wait(timeout=20)
        except subprocess.TimeoutExpired:
            proc.kill()
            rc = proc.wait()
    wall_ms = round((time.perf_counter() - t0) * 1000.0, 1)
    if timed_out:
        rc = 124
    stop_scope(unit)
    host_end = {"loadavg1": loadavg1(), "mem_available_mib": meminfo_mib()}
    # The run is over: merge the harness logs into the run's own directory.
    # When verify created it this is a no-op for ownership; when verify failed
    # before writing anything (no JSON envelope) the logs are the only evidence
    # and still land beside the row.
    os.makedirs(adir, exist_ok=True)
    stdout_path = shutil.move(stdout_path, os.path.join(adir, "stdout.json"))
    stderr_path = shutil.move(stderr_path, os.path.join(adir, "stderr.txt"))

    doc = {}
    try:
        with open(stdout_path) as f:
            doc = json.load(f)
    except Exception:
        doc = {}
    verdict_obj = ((doc.get("result") or {}).get("verdict") or {})
    steps = verdict_obj.get("steps") or []
    status = doc.get("status") or verdict_obj.get("verdict")
    if rc == 124:
        # Our own wall clock killed it: an infrastructure timeout, never a
        # verdict about the app or the scenario.
        verdict = "error"
        err = benchlib.classify_error("TIMEOUT", deadline="harness",
                                      message="exceeded --timeout-s %d" % TIMEOUT_S)
    elif not doc:
        verdict = "error"
        tail = ""
        try:
            with open(stderr_path, errors="replace") as f:
                tail = f.read()[-500:]
        except OSError:
            pass
        err = benchlib.classify_error("INTERNAL",
                                      message="no JSON envelope (rc=%s): %s" % (rc, tail))
    elif status == "pass":
        verdict = "pass"
        err = None
    elif status in ("fail", "timeout"):
        # A verify-reported `timeout` is a verdict, not a harness error: the run
        # finished and answered "this did not hold within its own deadline".
        verdict = "fail"
        e = doc.get("error") or (verdict_obj.get("failure") or {}).get("error") or {}
        failure = verdict_obj.get("failure") or {}
        err = benchlib.classify_error(e.get("code") or failure.get("code"),
                                      message=e.get("message"),
                                      hint=e.get("hint"))
        err["step_index"] = failure.get("step_index")
        err["reason"] = ((e.get("details") or {}).get("reason")
                         or err.get("reason"))
    elif status == "unsupported":
        # Only a scenario declared `unsupported` may call this a correct
        # non-pass; the summary's negative_is_correct decides from the pin.
        verdict = "fail"
        e = doc.get("error") or {}
        err = benchlib.classify_error(e.get("code") or "UNSUPPORTED_BACKEND",
                                      message=e.get("message"), hint=e.get("hint"))
        err["reason"] = (e.get("details") or {}).get("reason")
    else:
        verdict = "error"
        e = doc.get("error") or {}
        err = benchlib.classify_error(e.get("code") or "INTERNAL",
                                      message=e.get("message"), hint=e.get("hint"))
        err["reason"] = (e.get("details") or {}).get("reason")

    false_pass = bool(meta["expected"] == "fail" and verdict == "pass")
    false_fail = bool(meta["expected"] == "pass" and verdict != "pass"
                      and err and err["class"] in ("harness", "infra_timeout", "host_guard"))
    cur_samples = [s for s in samples if s.get("bytes") is not None]
    cg_peak = (max(s["bytes"] for s in cur_samples) / MIB) if cur_samples else None
    cg_gib_min = benchlib.sample_integral(cur_samples) if cur_samples else None
    pss_vals = [s["pss_mib"] for s in samples if s.get("pss_mib") is not None]
    pss_peak = max(pss_vals) if pss_vals else None
    first_frame_ms = first_frame_from_steps(steps)
    anon_settled = None
    if SETTLE_S > 0 and first_frame_ms is not None and samples:
        target = samples[0]["t"] + (first_frame_ms / 1000.0) + SETTLE_S
        if samples[-1]["t"] >= target:
            best = min((s for s in samples if s.get("anon_shmem_bytes") is not None),
                       key=lambda s: abs(s["t"] - target), default=None)
            if best is not None:
                anon_settled = best["anon_shmem_bytes"] / MIB
    acos_mem = {"value_mib": anon_settled, "metric_type": "CGROUP_ANON_SHMEM",
                "state": "cold-process", "settle_s": SETTLE_S if anon_settled is not None else None}
    legacy_cmp = benchlib.compare_memory(acos_mem, benchlib.legacy_memory_reference())
    return {
        "run_id": rid,
        "utc": utc(),
        "git": benchlib._git(ROOT, "rev-parse", "HEAD"),
        "dirty_hash": benchlib.repo_dirty_hash(ROOT)[0],
        "profile": PROFILE,
        "verdict_eligible": VERDICT_ELIGIBLE,
        "engine": ENGINE_LABEL,
        "scenario": scenario,
        "scenario_sha256": meta["sha256"],
        "apk_sha256": (verdict_obj.get("app") or {}).get("sha256") or sha256(meta["app"]),
        "variant": meta["variant"],
        "expected": meta["expected"],
        "expectation": meta["expectation"],
        "expect_error": meta["expect_error"],
        "needs_env": meta["needs_env"],
        "verdict": verdict,
        "false_pass": false_pass,
        "false_fail": false_fail,
        "negative": meta["variant"] == "negative",
        "error": err,
        "state": "cold-process" if ENGINE_LABEL == "art" else "unknown",
        "n_concurrent": 1,
        "wall_ms": wall_ms,
        "guest_wall_ms": (doc.get("metrics") or {}).get("wall_ms"),
        "first_frame_ms": first_frame_ms,
        "first_frame_ms_source": "cumulative step duration_ms through the first step with frame_seq",
        "steps": [{"kind": st.get("step"), "ms": st.get("duration_ms"),
                   "status": st.get("status")} for st in steps],
        "mem": {
            "cgroup_peak_mib": round(cg_peak, 2) if cg_peak is not None else None,
            "cgroup_gib_min": round(cg_gib_min, 6) if cg_gib_min is not None else None,
            "anon_shmem_settled_mib": round(anon_settled, 2) if anon_settled is not None else None,
            "pss_mib": round(pss_peak, 2) if pss_peak is not None else None,
            "settle_s": acos_mem["settle_s"],
            "metric_type": "CGROUP_ANON_SHMEM",
            "peak_type": "CGROUP_MEMORY_CURRENT",
            "pss_type": "PSS",
            "legacy_comparison": legacy_cmp,
            "sample_count": len(samples),
            "cgroup_source": cg_source if cg_path else "unavailable",
        },
        "host": {
            "loadavg1": host_start["loadavg1"],
            "loadavg5": None,
            "mem_available_mib": host_start["mem_available_mib"],
            "mem_available_end_mib": host_end["mem_available_mib"],
            "contaminated": bool((host_start["loadavg1"] or 0) > 4.0),
        },
        "artifacts": os.path.relpath(adir, ROOT),
        "art_engine_flag": ENGINE_FLAG,
        "rc": rc,
    }


def tsv(rows):
    cols = ["run_id", "scenario", "variant", "expected", "verdict", "false_pass",
            "error.class", "error.code", "wall_ms", "first_frame_ms",
            "mem.cgroup_peak_mib", "mem.cgroup_gib_min", "mem.pss_mib",
            "mem.anon_shmem_settled_mib", "host.loadavg1",
            "host.mem_available_mib"]
    lines = ["\t".join(cols)]
    for r in rows:
        e = r.get("error") or {}
        vals = [r["run_id"], r["scenario"], r["variant"], r["expected"], r["verdict"],
                str(r["false_pass"]).lower(), e.get("class", ""), e.get("code", ""),
                str(r["wall_ms"]), str(r.get("first_frame_ms")), str(r["mem"]["cgroup_peak_mib"]),
                str(r["mem"]["cgroup_gib_min"]), str(r["mem"]["pss_mib"]),
                str(r["mem"]["anon_shmem_settled_mib"]), str(r["host"]["loadavg1"]),
                str(r["host"]["mem_available_mib"])]
        lines.append("\t".join(vals))
    return "\n".join(lines) + "\n"


def main():
    stamp = os.path.splitext(os.path.basename(OUT))[0]
    rows = []
    idx = 0
    skipped = []
    for scenario in SCENARIOS:
        meta = read_scenario(scenario)
        # `needs_env` is a declared host prerequisite, not an excuse: the
        # scenario is not run at all when the capability is missing, and the
        # matrix says so instead of turning it into a red row that is not the
        # scenario's fault (net-check/network:real need the pinned pasta).
        missing = [name for name in meta["needs_env"] if not needs_env_present(name)]
        if missing:
            skipped.append({"scenario": scenario, "needs_env": missing,
                            "reason": "host capability missing: " + ", ".join(missing)})
            print("run-matrix: SKIP %s: needs %s (not present)"
                  % (scenario, ", ".join(missing)), file=sys.stderr)
            continue
        for rep in range(1, REPS + 1):
            idx += 1
            print("run-matrix: [%d] %s rep %d/%d" % (idx, scenario, rep, REPS),
                  file=sys.stderr)
            row = run_one(scenario, rep, idx)
            print("run-matrix: [%d] verdict=%s class=%s wall=%.0f ms cg_peak=%s MiB"
                  % (idx, row["verdict"], (row["error"] or {}).get("class", "-"),
                     row["wall_ms"], row["mem"]["cgroup_peak_mib"]), file=sys.stderr)
            rows.append(row)

    classes = {}
    for r in rows:
        c = (r["error"] or {}).get("class", "pass" if r["verdict"] == "pass" else "unknown")
        classes[c] = classes.get(c, 0) + 1
    false_pass = sum(1 for r in rows if r["false_pass"])
    false_fail = sum(1 for r in rows if r["false_fail"])
    negative = [r for r in rows if r["negative"]]
    unclassified = sum(1 for r in rows if (r["error"] or {}).get("class") == "unknown")
    # Only the harness's own failures count against the harness gate. A
    # scenario's own step deadline (`step_timeout`) is a real non-pass, and a
    # negative that declares that timeout is a correct negative.
    harness = (classes.get("harness", 0) + classes.get("infra_timeout", 0)
               + classes.get("host_guard", 0))
    negative_correct = sum(1 for r in negative if negative_is_correct(r, r))
    negative_pin_mismatch = [
        r["run_id"] for r in negative
        if r.get("expect_error") and declared_failure_matches(r, r) is False
    ]
    summary = {
        "total_runs": len(rows),
        "passed": sum(1 for r in rows if r["verdict"] == "pass"),
        "failed": sum(1 for r in rows if r["verdict"] == "fail"),
        "errors": sum(1 for r in rows if r["verdict"] == "error"),
        "false_pass": false_pass,
        "false_fail": false_fail,
        "negative_runs": len(negative),
        "negative_correct": negative_correct,
        "negative_expect_error_mismatch": negative_pin_mismatch,
        "unclassified": unclassified,
        "harness_or_timeout": harness,
        "infra_timeout": classes.get("infra_timeout", 0),
        "step_timeout": classes.get("step_timeout", 0),
        "skipped_needs_env": skipped,
        "classes": classes,
        "verdict_eligible": VERDICT_ELIGIBLE,
        "gates": {
            "false_pass_zero": false_pass == 0,
            "unclassified_zero": unclassified == 0,
            "harness_timeout_within_2pct": harness <= max(1, int(0.02 * len(rows) + 0.999)),
            "negative_all_correct": negative_correct == len(negative),
            "no_infra_timeout": classes.get("infra_timeout", 0) == 0,
        },
    }
    result = {
        "schema": benchlib.MATRIX_SCHEMA,
        "utc": utc(),
        "provenance": benchlib.provenance(
            ROOT, [ACOS, ACOS_ART], profile=PROFILE,
            gate=benchlib.host_gate(ROOT, check_lock=False),
            engine=ENGINE_LABEL,
            extra={"artifacts_dir": os.path.relpath(ARTIFACTS, ROOT),
                   "sample_ms": SAMPLE_MS, "settle_s": SETTLE_S,
                   "engine_flag": ENGINE_FLAG}),
        "legacy_reference": benchlib.legacy_memory_reference(),
        "runs": rows,
        "summary": summary,
    }
    tsv_path = os.path.splitext(OUT)[0] + ".tsv"
    with open(tsv_path, "w") as f:
        f.write(tsv(rows))
    errors = benchlib.validate_matrix(result)
    if errors:
        bad = OUT + ".invalid.json"
        benchlib.write_json(bad, result)
        print("run-matrix: result INVALID (%d schema/gate errors); wrote %s"
              % (len(errors), bad), file=sys.stderr)
        for e in errors:
            print("run-matrix:   " + e, file=sys.stderr)
        return 1
    benchlib.write_json(OUT, result)
    print("run-matrix: summary %s" % json.dumps(summary, sort_keys=True))
    print("run-matrix: rows %s" % OUT)
    print("run-matrix: tsv  %s" % tsv_path)
    print("run-matrix: verdict_eligible=%s profile=%s engine=%s"
          % (VERDICT_ELIGIBLE, PROFILE, ENGINE_LABEL))
    print("")
    print("| run | scenario | variant | expected | verdict | false_pass | error class | wall ms | cg peak MiB | GiB-min | PSS MiB |")
    print("|---|---|---|---|---|---|---|---|---|---|---|")
    for r in rows:
        e = r["error"] or {}
        print("| %s | %s | %s | %s | %s | %s | %s | %.0f | %s | %s | %s |" % (
            r["run_id"], os.path.basename(r["scenario"]), r["variant"], r["expected"],
            r["verdict"], r["false_pass"], e.get("class", "-"), r["wall_ms"],
            r["mem"]["cgroup_peak_mib"], r["mem"]["cgroup_gib_min"], r["mem"]["pss_mib"]))
    return 0 if not false_pass and unclassified == 0 else 1


sys.exit(main())
