#!/usr/bin/env bash
#
# run-matrix.sh -- the ACOS scenario matrix on ONE result schema (W29).
#
#   tools/verify/run-matrix.sh [options]
#
# Options
#   --scenario FILE        scenario to run (repeatable). Default: the ART counter
#                          scenario plus EVERY scenarios/negative/*.yaml.
#   --scenarios "A B"      space-separated scenario list (same as repeated --scenario)
#   --modes good|neg|all   default all (the explicit --scenario list wins)
#   --apk APK              override the scenario app (passed as --apk)
#   --reps N               reps per scenario (default 1); each rep is a separate row
#   --engine art|none|auto auto (default): pass --engine art when the binary
#                          supports it, else record engine "default" honestly
#   --profile P            release (default) | debug. A debug run without
#                          --allow-debug is refused; with it every row is
#                          NOT_VERDICT_ELIGIBLE.
#   --target-dir DIR       cargo target dir (default $CARGO_TARGET_DIR or target)
#   --allow-debug          permit a debug build, labelled
#   --allow-contaminated   run even when the host gate refuses (loadavg1 > 4 /
#                          MemAvailable < 30 GiB), for CORRECTNESS classification
#                          only: every row and the provenance are labelled
#                          verdict_eligible=false, so no timing numbers may be
#                          claimed from it. Also ACOS_MATRIX_ALLOW_CONTAMINATED=1.
#   --no-build             reuse existing binaries (the fixture APKs a selected
#                          scenario needs are still built on demand from
#                          fixtures/apps via tools/build/build-apps.sh)
#   --sample-ms MS         cgroup/PSS sampling period (default 100)
#   --settle-s S           seconds after first frame at which the legacy-comparable
#                          cgroup anon+shmem value is read (default 0 = not read;
#                          legacy comparison is then NOT_COMPARABLE). The legacy
#                          reference requires 60.
#   --timeout-s S          per-run timeout (default 300)
#   --artifacts DIR        base artifacts dir (default .cache/bench-matrix/<stamp>)
#   --out FILE             default results/bench/matrix-<stamp>.json
#   --list                 print the selected scenarios and exit (no build, no ART)
#
# Release gate (owner direction 2026-11-18 §2): this matrix is the CORRECTNESS
# half. The TIMING half must also show that ACOS beats the stock Android emulator
# on every tracked metric -- run
#   tools/art-runtime/emubench/emu_gate.py --acos <gate timing JSON> \
#       --emu results/bench/emubench-2026-11-18.json
# and require 0 metrics worse for ACOS (BENCHMARKS.md section 13).
#
# What each row carries: wall ms, cgroup memory.current peak + GiB-min integral
# (sampled every 100 ms in the run's own systemd scope), PSS (secondary), the
# cgroup anon+shmem value at --settle-s, the structured error {code,class,
# retryable,hint}, and the negative-scenarios column (variant/expected/
# false_pass). Schema: results/bench/schema/matrix.schema.json.
#
# Gates (tools/bench/benchlib.py validate_matrix): false_pass must be 0 and
# unclassified errors must be 0, or the run exits 1 and writes
# <out>.invalid.json instead of pretending the matrix is valid.
#
# Safety: builds through tools/guard/guarded.sh; the measurement runs through
# tools/art-runtime/run-exclusive.sh --task W29-matrix -- tools/guard/guarded.sh
# --mem 8G --time 900 -- python3 ...  (one ART guest at a time host-wide).
# Scratch lives under .cache/ (never /tmp).
set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT" || exit 1

usage() { sed -n '2,50p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'; }

STAMP="$(date -u +%Y%m%dT%H%M%SZ)"
PROFILE="${ACOS_BENCH_PROFILE:-release}"
TARGET_DIR="${CARGO_TARGET_DIR:-target}"
ALLOW_DEBUG=0
ALLOW_CONTAMINATED="${ACOS_MATRIX_ALLOW_CONTAMINATED:-0}"
NO_BUILD=0
ENGINE="auto"
REPS=1
SAMPLE_MS=100
SETTLE_S=0
TIMEOUT_S=300
APK_OVERRIDE=""
ARTIFACTS=""
OUT="results/bench/matrix-${STAMP}.json"
LIST=0
SCENARIOS=()

while [ $# -gt 0 ]; do
    case "$1" in
        --scenario)     SCENARIOS+=("$2"); shift 2 ;;
        --scenarios)    read -r -a _more <<<"$2"; SCENARIOS+=("${_more[@]}"); shift 2 ;;
        --modes)        case "$2" in all|good|neg) ;; *) echo "run-matrix: --modes must be all|good|neg" >&2; exit 2 ;; esac
                        MODES="$2"; shift 2 ;;
        --apk)          APK_OVERRIDE="$2"; shift 2 ;;
        --reps)         REPS="$2"; shift 2 ;;
        --engine)       ENGINE="$2"; shift 2 ;;
        --profile)      PROFILE="$2"; shift 2 ;;
        --target-dir)   TARGET_DIR="$2"; shift 2 ;;
        --allow-debug)  ALLOW_DEBUG=1; shift ;;
        --allow-contaminated) ALLOW_CONTAMINATED=1; shift ;;
        --no-build)     NO_BUILD=1; shift ;;
        --sample-ms)    SAMPLE_MS="$2"; shift 2 ;;
        --settle-s)     SETTLE_S="$2"; shift 2 ;;
        --timeout-s)    TIMEOUT_S="$2"; shift 2 ;;
        --artifacts)    ARTIFACTS="$2"; shift 2 ;;
        --out)          OUT="$2"; shift 2 ;;
        --list)         LIST=1; shift ;;
        -h|--help)      usage; exit 0 ;;
        *) echo "run-matrix: unknown argument: $1" >&2; exit 2 ;;
    esac
done
MODES="${MODES:-all}"

case "$PROFILE" in release|debug) ;; *) echo "run-matrix: --profile must be release|debug" >&2; exit 2 ;; esac
case "$ENGINE" in auto|art|none) ;; *) echo "run-matrix: --engine must be auto|art|none" >&2; exit 2 ;; esac
case "$REPS$SAMPLE_MS$SETTLE_S$TIMEOUT_S" in *[!0-9]*) echo "run-matrix: numeric options must be integers" >&2; exit 2 ;; esac
[ "$REPS" -ge 1 ] || { echo "run-matrix: --reps must be >= 1" >&2; exit 2; }

case "$TARGET_DIR" in /*) BIN_ROOT="$TARGET_DIR" ;; *) BIN_ROOT="$ROOT/$TARGET_DIR" ;; esac
ACOS="$BIN_ROOT/$PROFILE/acos"
ACOS_ART="$BIN_ROOT/$PROFILE/acos-art"

# ---- default scenario set: the ART counter path + every negative twin ----
if [ "${#SCENARIOS[@]}" -eq 0 ]; then
    case "$MODES" in
        neg)  SCENARIOS=()
              while IFS= read -r f; do SCENARIOS+=("$f"); done < <(ls scenarios/negative/*.yaml 2>/dev/null | sort) ;;
        good) SCENARIOS=("scenarios/v2-art-counter.yaml") ;;
        all)  SCENARIOS=("scenarios/v2-art-counter.yaml")
              while IFS= read -r f; do SCENARIOS+=("$f"); done < <(ls scenarios/negative/*.yaml 2>/dev/null | sort) ;;
    esac
fi
[ "${#SCENARIOS[@]}" -gt 0 ] || { echo "run-matrix: no scenarios selected" >&2; exit 2; }
for s in "${SCENARIOS[@]}"; do
    [ -f "$s" ] || { echo "run-matrix: scenario not found: $s" >&2; exit 2; }
done

if [ "$LIST" -eq 1 ]; then
    printf '%s\n' "${SCENARIOS[@]}"
    exit 0
fi

if [ "$PROFILE" != "release" ] && [ "$ALLOW_DEBUG" -eq 0 ]; then
    echo "run-matrix: refusing a $PROFILE build (verdicts are release-only)." >&2
    echo "run-matrix: use --profile release, or --allow-debug to label rows NOT_VERDICT_ELIGIBLE." >&2
    exit 5
fi
[ -n "$ARTIFACTS" ] || ARTIFACTS="$ROOT/.cache/bench-matrix/$STAMP"
mkdir -p "$ARTIFACTS" "$(dirname "$OUT")"

# ---- scenario fixtures: build what the selected scenarios need --------------
# The default matrix set runs the fixtures under fixtures/build/apps (counter,
# finish-start, form-validation, offscreen-pager, ...). A `fixtures/build` copied
# from another worktree can be stale: the v0.3.0 release gate lost
# `negative/finish-start-wrong-home` to `APK_IO` because `finish-start.apk` was
# not in the main checkout's fixture build (gate finding F3), which reads as an
# unclassified row even though nothing is wrong. Build the missing ones from
# their `fixtures/apps/<name>` sources (guarded) before the run starts, and
# refuse with an actionable message when a fixture has no builder here.
missing_fixtures=()
while IFS= read -r app; do
    [ -n "$app" ] || continue
    case "$app" in fixtures/build/apps/*.apk) ;; *) continue ;; esac
    [ -f "$ROOT/$app" ] && continue
    name="$(basename "$app" .apk)"
    missing_fixtures+=("$name")
done < <(sed -n 's/^app:[[:space:]]*//p' "${SCENARIOS[@]}" | tr -d '"\047' | sort -u)
if [ "${#missing_fixtures[@]}" -gt 0 ]; then
    buildable=()
    for name in "${missing_fixtures[@]}"; do
        if [ -d "$ROOT/fixtures/apps/$name" ]; then
            buildable+=("$name")
        fi
    done
    if [ "${#buildable[@]}" -gt 0 ]; then
        echo "run-matrix: building missing fixture APK(s): ${buildable[*]}" >&2
        if ! ACOS_APP_ONLY="${buildable[*]}" "$ROOT/tools/build/build-apps.sh"; then
            echo "run-matrix: fixture build failed; no run" >&2
            exit 7
        fi
    fi
    unresolved=()
    for name in "${missing_fixtures[@]}"; do
        case " ${buildable[*]-} " in *" $name "*) ;; *) unresolved+=("$name") ;; esac
    done
    if [ "${#unresolved[@]}" -gt 0 ]; then
        echo "run-matrix: missing fixture APK(s) with no tools/build/build-apps.sh source:" \
             "${unresolved[*]}" >&2
        echo "run-matrix: build them first (see fixtures/README.md; e.g." \
             "fixtures/build-hostile-probe.sh for hostile-*.apk) or run a" \
             "--scenarios subset that does not need them." >&2
        exit 7
    fi
    still_missing=()
    for name in "${buildable[@]}"; do
        [ -f "$ROOT/fixtures/build/apps/$name.apk" ] || still_missing+=("$name")
    done
    if [ "${#still_missing[@]}" -gt 0 ]; then
        echo "run-matrix: fixture build did not produce: ${still_missing[*]}" >&2
        exit 7
    fi
fi

# ---- host gate before the build (loadavg1 <= 4.0, MemAvailable >= 30 GiB) ----
CONTAMINATED=0
if ! python3 -I - "$ROOT" <<'PY'
import sys
sys.path.insert(0, sys.argv[1] + "/tools/bench")
import benchlib
g = benchlib.host_gate(sys.argv[1], check_lock=False)
print("run-matrix: host gate %s (loadavg1 %s, MemAvailable %s GiB)"
      % ("ok" if g["ok"] else "REFUSED", g["loadavg1"], g["mem_available_gib"]),
      file=sys.stderr)
if not g["ok"]:
    for r in g["reasons"]:
        print("run-matrix:   " + r, file=sys.stderr)
    sys.exit(1)
PY
then
    if [ "$ALLOW_CONTAMINATED" != "1" ]; then
        exit 4
    fi
    CONTAMINATED=1
    echo "run-matrix: --allow-contaminated: continuing on a busy host for" \
         "CORRECTNESS classification only; every row is verdict_eligible=false" \
         "(no timing claim may be made from this run)." >&2
fi

# ---- build (guarded, release unless --profile debug) ----
BUILD_ARGS=(-p acos-cli -p acos-art)
[ "$PROFILE" = release ] && BUILD_ARGS=(--release "${BUILD_ARGS[@]}")
if [ "$NO_BUILD" -eq 0 ]; then
    if ! CARGO_TARGET_DIR="$TARGET_DIR" "$ROOT/tools/guard/guarded.sh" --mem 8G --time 900 -- \
            cargo build "${BUILD_ARGS[@]}"; then
        echo "run-matrix: build failed; no run" >&2
        exit 3
    fi
fi
for b in "$ACOS" "$ACOS_ART"; do
    [ -x "$b" ] || { echo "run-matrix: $b missing" >&2; exit 3; }
done

# ---- one ART run at a time: hold the host-wide lock for the whole matrix ----
# The driver is written to the artifacts dir (not piped), because the lock and
# guard wrappers must not have to preserve a heredoc on stdin.
cat >"$ARTIFACTS/driver.py" <<'PY'
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
sys.path.insert(0, os.path.join(ROOT, "tools", "bench"))
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
#
# The answer is the *product's* rule, not a second one: `acos doctor --json`
# reports the `pasta` check from `crates/acos-netd-ctl/src/pasta.rs` (the same
# lookup the relay supervisor and `tools/release/install.sh` use, which includes
# `<runtime root>/libexec/pasta`). A row that is skipped therefore means "this
# host cannot run the arm honestly", which is exactly what `needs_env` declares.
_NEEDS_ENV_CACHE = {}


def doctor_capabilities():
    """Capabilities `acos doctor` says are present, asked once per run."""
    if "caps" in _NEEDS_ENV_CACHE:
        return _NEEDS_ENV_CACHE["caps"]
    caps = {}
    try:
        out = subprocess.run([ACOS, "doctor", "--json"], capture_output=True,
                             text=True, timeout=120)
        report = json.loads(out.stdout or "{}")
        for check in report.get("checks") or []:
            caps[check.get("name")] = (check.get("status") == "ok",
                                       check.get("detail") or "")
    except Exception as exc:  # noqa: BLE001 - a missing answer is "not present"
        caps["__error__"] = (False, str(exc))
    _NEEDS_ENV_CACHE["caps"] = caps
    return caps


def needs_env_present(name):
    if name == "pasta":
        present, _ = doctor_capabilities().get("pasta", (False, ""))
        return present
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
            # Say what the product said, not just "not present": `acos doctor`'s
            # own line for the capability is the reason (e.g. `pasta: missing
            # (searched: …)`), so the next action is in the report.
            said = {name: doctor_capabilities().get(name, (False, ""))[1]
                    for name in missing}
            skipped.append({"scenario": scenario, "needs_env": missing,
                            "doctor": said,
                            "reason": "host capability missing: " + ", ".join(missing)})
            print("run-matrix: SKIP %s: needs %s (not present: %s)"
                  % (scenario, ", ".join(missing),
                     "; ".join(f"{k}: {v}" for k, v in said.items())), file=sys.stderr)
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
PY

exec "$ROOT/tools/art-runtime/run-exclusive.sh" --task "W29-matrix" -- \
    "$ROOT/tools/guard/guarded.sh" --mem 8G --time 900 -- \
    python3 -I "$ARTIFACTS/driver.py" "$ROOT" "$OUT" "$PROFILE" "$ACOS" "$ACOS_ART" \
    "$ENGINE" "$REPS" "$SAMPLE_MS" "$SETTLE_S" "$TIMEOUT_S" "$ARTIFACTS" \
    "$APK_OVERRIDE" "$CONTAMINATED" "${SCENARIOS[@]}"
