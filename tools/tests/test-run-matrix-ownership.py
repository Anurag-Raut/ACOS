#!/usr/bin/env python3
"""tools/verify/run-matrix.sh: the harness must not own the artifacts directory.

`acos verify --artifacts <dir>` owns that directory (ADR-0026, one run owns one
artifacts directory): an existing directory with files in it is another run's
evidence and is refused with INVALID_ARGUMENT. `run-matrix.sh` used to
`mkdir` the row directory and write its own `stdout.json` into it *before*
launching verify, so every row came back `verdict=error class=harness`
(REL3P, `results/handoff/REL3P/art/matrix-counter-gaten.json`).

This test extracts the driver the script actually embeds, runs it against a
stub `acos` that enforces the same ownership rule, and requires both rows to be
real passes. Before the fix the stub sees a non-empty directory and both rows
are harness errors, so the test fails; after it, verify creates the directory
and the harness moves its logs in afterwards.

Run: python3 tools/tests/test-run-matrix-ownership.py
"""
import json
import os
import re
import shutil
import stat
import subprocess
import sys
import tempfile

HERE = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.dirname(os.path.dirname(HERE))
SCRIPT = os.path.join(REPO, "tools", "verify", "run-matrix.sh")

PASSES = FAILURES = 0


def check(name, cond, detail=""):
    global PASSES, FAILURES
    if cond:
        PASSES += 1
        print("PASS %s" % name)
    else:
        FAILURES += 1
        print("FAIL %s%s" % (name, (" -- " + detail) if detail else ""))


def extract_driver(text):
    """The `cat >"$ARTIFACTS/driver.py" <<'PY' ... PY` heredoc body."""
    m = re.search(r"cat >\"\$ARTIFACTS/driver\.py\" <<'PY'\n(.*?)\nPY\n", text, re.S)
    if not m:
        raise SystemExit("run-matrix.sh: driver heredoc not found")
    return m.group(1)


STUB_ACOS = """#!/usr/bin/env bash
# Stub of `acos verify` with the real artifacts-ownership rule.
set -uo pipefail
if [ "${1:-}" = "verify" ] && [ "${2:-}" = "--help" ]; then
    echo "Usage: acos verify <scenario> [--json] [--artifacts DIR]"
    exit 0
fi
scenario=""
adir=""
while [ $# -gt 0 ]; do
    case "$1" in
        --artifacts) adir="$2"; shift 2 ;;
        --engine) shift 2 ;;
        --apk) shift 2 ;;
        --help) echo "Usage: acos verify"; exit 0 ;;
        -*) shift ;;
        *) scenario="$1"; shift ;;
    esac
done
if [ -n "$adir" ] && [ -d "$adir" ] && [ -n "$(ls -A "$adir" 2>/dev/null)" ]; then
    printf '{"ok":false,"status":"error","error":{"code":"INVALID_ARGUMENT","message":"artifacts directory already holds an incomplete earlier run, so this run may not write into it"}}'
    exit 1
fi
[ -n "$adir" ] && mkdir -p "$adir" && printf '{"request_id":"stub"}' > "$adir/run.json"
printf '{"ok":true,"status":"pass","result":{"verdict":{"steps":[{"step":"launch","duration_ms":1.5,"frame_seq":1}]},"app":{"sha256":"deadbeef"}},"metrics":{"wall_ms":1.5}}'
exit 0
"""

STUB_SYSTEMD_RUN = """#!/usr/bin/env bash
# Drop systemd-run's own flags and run the command directly (no user manager
# in the test environment).
while [ $# -gt 0 ]; do
    case "$1" in
        --unit) shift 2 ;;
        --property=*) shift ;;
        --user|--scope|--quiet|--collect) shift ;;
        *) break ;;
    esac
done
exec "$@"
"""


def write_exec(path, text):
    with open(path, "w") as f:
        f.write(text)
    os.chmod(path, os.stat(path).st_mode | stat.S_IXUSR | stat.S_IXGRP | stat.S_IXOTH)


def main():
    if not os.path.exists(SCRIPT):
        print("SKIP test-run-matrix-ownership: %s missing" % SCRIPT)
        return 0
    with open(SCRIPT) as f:
        driver = extract_driver(f.read())

    work = tempfile.mkdtemp(prefix="run-matrix-ownership-", dir=os.path.join(REPO, ".cache"))
    try:
        acos = os.path.join(work, "acos")
        acos_art = os.path.join(work, "acos-art")
        write_exec(acos, STUB_ACOS)
        write_exec(acos_art, "#!/usr/bin/env bash\nexit 0\n")
        bin_dir = os.path.join(work, "bin")
        os.makedirs(bin_dir)
        write_exec(os.path.join(bin_dir, "systemd-run"), STUB_SYSTEMD_RUN)

        scenario = os.path.join(work, "row.yaml")
        app = os.path.join(work, "app.apk")
        with open(app, "wb") as f:
            f.write(b"apk")
        with open(scenario, "w") as f:
            f.write(
                "version: 1\nname: ownership-row\napp: %s\nexpectation: pass\n"
                % os.path.relpath(app, REPO)
            )
        out = os.path.join(work, "matrix.json")
        artifacts = os.path.join(work, "artifacts")
        driver_py = os.path.join(work, "driver.py")
        with open(driver_py, "w") as f:
            f.write(driver)

        argv = [
            sys.executable, driver_py, REPO, out, "release", acos, acos_art, "auto",
            "1", "100", "0", "120", artifacts, "", scenario, scenario,
        ]
        env = dict(os.environ)
        env["PATH"] = bin_dir + os.pathsep + env.get("PATH", "")
        proc = subprocess.run(argv, capture_output=True, text=True, timeout=180, env=env)
        check("driver exited 0", proc.returncode == 0,
              "rc=%s stderr=%s" % (proc.returncode, proc.stderr[-800:]))

        try:
            with open(out) as f:
                matrix = json.load(f)
        except Exception as exc:  # noqa: BLE001
            matrix = None
            check("matrix written and valid", False,
                  "%s; stdout=%s stderr=%s" % (exc, proc.stdout[-800:], proc.stderr[-800:]))
        if matrix:
            rows = matrix.get("runs") or []
            check("two rows", len(rows) == 2, repr(len(rows)))
            for i, row in enumerate(rows):
                err = row.get("error") or {}
                check(
                    "row %d is a real verdict, not a harness error" % i,
                    row.get("verdict") == "pass" and err.get("class") != "harness",
                    "verdict=%s error=%s" % (row.get("verdict"), err),
                )
                check(
                    "row %d keeps the harness logs beside the run's evidence" % i,
                    os.path.exists(os.path.join(row["artifacts"].replace("./", ""),
                                                "stdout.json"))
                    or os.path.exists(os.path.join(REPO, row["artifacts"], "stdout.json")),
                    row.get("artifacts"),
                )
            summary = matrix.get("summary") or {}
            check("no harness/timeout rows", summary.get("harness_or_timeout") == 0,
                  repr(summary))
    finally:
        shutil.rmtree(work, ignore_errors=True)

    print("\n%d passed, %d failed" % (PASSES, FAILURES))
    return 0 if FAILURES == 0 else 1


if __name__ == "__main__":
    sys.exit(main())
