#!/usr/bin/env bash
#
# Differential runner for the ACOS DEX interpreter corpus.
#
# For every fixtures/build/dexprogs/<name>/classes.dex program this script runs
# `acos run-dex` (release build) and compares stdout byte-for-byte and the exit
# code against fixtures/expected/dexprogs/<name>.{stdout,exit} (jvm-reference).
#
# Usage:
#   tools/verify/diff-dexprogs.sh [--gc-stress] [--corelib bootstrap|real] [--expected jvm|art]
#
# --gc-stress sets ACOS_GC_STRESS=1 (a collection before every allocation,
# proving GC roots are exact).
# --corelib selects the class provider. With --corelib real the authoritative
# expectation is fixtures/expected-art/dexprogs (real ART); bootstrap compares
# against the JVM reference. --expected overrides that choice.
#
# Writes results/vm/dexprogs-<corelib>-<mode>.json and exits nonzero unless
# every program passes.
set -uo pipefail

# Fixture-dependent tests must fail, not silently SKIP, when run under CI
# (HONESTY_AUDIT.md #11). Develop locally with ACOS_REQUIRE_FIXTURES= to opt out.
export ACOS_REQUIRE_FIXTURES="${ACOS_REQUIRE_FIXTURES:-1}"

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
MODE="normal"
STRESS=0
CORELIB="bootstrap"
EXPECTED_KIND=""
while [ $# -gt 0 ]; do
    case "$1" in
        --gc-stress) MODE="gc-stress"; STRESS=1 ;;
        --corelib) CORELIB="$2"; shift ;;
        --expected) EXPECTED_KIND="$2"; shift ;;
        *) echo "diff-dexprogs: unknown argument: $1" >&2; exit 2 ;;
    esac
    shift
done
if [ -z "$EXPECTED_KIND" ]; then
    if [ "$CORELIB" = "real" ]; then EXPECTED_KIND="art"; else EXPECTED_KIND="jvm"; fi
fi
case "$EXPECTED_KIND" in
    jvm) EXPECTED="$ROOT/fixtures/expected/dexprogs" ;;
    art) EXPECTED="$ROOT/fixtures/expected-art/dexprogs" ;;
    *) echo "diff-dexprogs: unknown --expected $EXPECTED_KIND" >&2; exit 2 ;;
esac

# Resolve the acos binary. Honour an explicit ACOS_BIN override first, then
# CARGO_TARGET_DIR (the harness builds into a non-default target dir), and
# fall back to the workspace `target/release`. A relative CARGO_TARGET_DIR is
# resolved against the repository root, matching cargo's own behaviour when the
# script is invoked from anywhere.
if [ -n "${CARGO_TARGET_DIR:-}" ]; then
    case "$CARGO_TARGET_DIR" in
        /*) TARGET_ROOT="$CARGO_TARGET_DIR" ;;
        *) TARGET_ROOT="$ROOT/$CARGO_TARGET_DIR" ;;
    esac
else
    TARGET_ROOT="$ROOT/target"
fi
if [ -n "${ACOS_BIN:-}" ]; then
    BIN="$ACOS_BIN"
else
    BIN="$TARGET_ROOT/release/acos"
fi
FIX="$ROOT/fixtures/build/dexprogs"
OUTDIR="$ROOT/results/vm"
mkdir -p "$OUTDIR"

if [ ! -d "$FIX" ]; then
    echo "diff-dexprogs: error: $FIX is absent; run tools/build/build-fixtures.sh" >&2
    exit 2
fi
if [ ! -x "$BIN" ]; then
    echo "diff-dexprogs: building acos (release) -> $BIN..." >&2
    (cd "$ROOT" && "$ROOT/tools/guard/guarded.sh" --mem 6G --time 1200 -- \
        cargo build --release -p acos-cli) || exit 2
fi

# Scratch is kept under the target dir and removed on exit (ADR-0018).
mkdir -p "$TARGET_ROOT/tmp"
TMP="$(mktemp -d "$TARGET_ROOT/tmp/diff-dexprogs.XXXXXX")"
trap 'rm -rf "$TMP"' EXIT

echo "diff-dexprogs: mode=$MODE corelib=$CORELIB expected=$EXPECTED_KIND"
printf '%-22s %-12s %s\n' "PROGRAM" "STATUS" "DETAIL"
printf '%-22s %-12s %s\n' "----------------------" "------------" "------------------------------"

for dir in "$FIX"/*/; do
    name="$(basename "$dir")"
    exp_stdout="$EXPECTED/$name.stdout"
    exp_exit="$(cat "$EXPECTED/$name.exit" 2>/dev/null || echo 0)"
    if [ "$STRESS" = "1" ]; then
        ACOS_GC_STRESS=1 "$ROOT/tools/guard/guarded.sh" --mem 2G --time 60 -- \
            "$BIN" run-dex "$dir/classes.dex" --main Main \
            --corelib "$CORELIB" --json >"$TMP/$name.json" 2>"$TMP/$name.err"
    else
        "$ROOT/tools/guard/guarded.sh" --mem 2G --time 60 -- \
            "$BIN" run-dex "$dir/classes.dex" --main Main \
            --corelib "$CORELIB" --json >"$TMP/$name.json" 2>"$TMP/$name.err"
    fi
    exp_stderr="$EXPECTED/$name.stderr"
    [ -f "$exp_stderr" ] || exp_stderr=""
    printf '%s\t%s\t%s\t%s\n' "$name" "$exp_stdout" "$exp_exit" \
        "$exp_stderr" >>"$TMP/index.tsv"
done

python3 - "$TMP" "$OUTDIR" "$MODE" "$STRESS" "$CORELIB" <<'PY'
import json, os, sys

tmp, outdir, mode, stress, corelib = (
    sys.argv[1], sys.argv[2], sys.argv[3], sys.argv[4] == "1", sys.argv[5]
)

def first_diff(got, want):
    gl, wl = got.splitlines(), want.splitlines()
    for i in range(max(len(gl), len(wl))):
        g = gl[i] if i < len(gl) else "<eof>"
        w = wl[i] if i < len(wl) else "<eof>"
        if g != w:
            return f"line {i+1}: got {g!r} want {w!r}"
    return "outputs equal"

records = []
counts = {"pass": 0, "fail": 0, "unsupported": 0}
for line in open(os.path.join(tmp, "index.tsv")):
    name, exp_stdout_path, exp_exit_s, exp_stderr_path = line.rstrip("\n").split("\t")
    exp_exit = int(exp_exit_s)
    want = open(exp_stdout_path, "r", encoding="utf-8", errors="replace").read()
    try:
        env = json.load(open(os.path.join(tmp, name + ".json")))
    except Exception as e:
        records.append({"program": name, "status": "fail", "detail": f"bad json: {e}"})
        counts["fail"] += 1
        print(f"{name:<22} {'FAIL':<12} bad json envelope")
        continue
    err = env.get("error") or {}
    code = err.get("code", "")
    if code.startswith("UNSUPPORTED"):
        counts["unsupported"] += 1
        detail = err.get("message", "")
        det = err.get("details") or {}
        api = det.get("api") or det.get("opcode")
        if api:
            detail = f"{api} ({detail})"
        records.append({"program": name, "status": "unsupported", "detail": detail,
                        "error": err, "metrics": env.get("metrics")})
        print(f"{name:<22} {'UNSUPPORTED':<12} {detail[:70]}")
        continue
    result = env.get("result") or {}
    got = result.get("stdout", "")
    exit_code = result.get("exit_code")
    # For uncaught-exception programs the oracle only pins the first stderr
    # line (the stack-frame text is VM-specific).
    stderr_ok = True
    stderr_detail = ""
    if exp_stderr_path:
        want_err = open(exp_stderr_path, "r", encoding="utf-8",
                        errors="replace").read().splitlines()
        got_err = result.get("stderr", "").splitlines()
        if want_err and (not got_err or got_err[0] != want_err[0]):
            stderr_ok = False
            stderr_detail = (f"stderr line 1: got {got_err[0]!r}"
                             if got_err else "stderr empty")
    if got == want and exit_code == exp_exit and stderr_ok:
        counts["pass"] += 1
        records.append({"program": name, "status": "pass",
                        "exit_code": exit_code, "metrics": env.get("metrics")})
        print(f"{name:<22} {'PASS':<12} exit={exit_code} stdout={len(got)} bytes")
    else:
        counts["fail"] += 1
        if exit_code != exp_exit:
            detail = f"exit {exit_code} (want {exp_exit})"
        elif not stderr_ok:
            detail = stderr_detail
        else:
            detail = first_diff(got, want)
        uncaught = result.get("uncaught_exception")
        if uncaught:
            detail = f"uncaught {uncaught.get('class')}: {uncaught.get('message')}; {detail}"
        if code:
            detail = f"{code}: {err.get('message')}; {detail}"
        records.append({"program": name, "status": "fail", "detail": detail,
                        "exit_code": exit_code, "metrics": env.get("metrics")})
        print(f"{name:<22} {'FAIL':<12} {detail[:90]}")

total = len(records)
report = {"mode": mode, "corelib": corelib, "gc_stress": stress, "total": total,
          "pass": counts["pass"], "fail": counts["fail"],
          "unsupported": counts["unsupported"], "programs": records}
path = os.path.join(outdir, f"dexprogs-{corelib}-{mode}.json")
with open(path, "w") as f:
    json.dump(report, f, indent=2)
print()
print(f"dexprogs: {counts['pass']} pass / {counts['fail']} fail / "
      f"{counts['unsupported']} unsupported of {total}")
print(f"dexprogs: report -> {path}")
sys.exit(0 if counts["pass"] == total else 1)
PY
