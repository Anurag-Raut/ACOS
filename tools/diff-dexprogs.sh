#!/usr/bin/env bash
#
# Differential runner for the ACOS DEX interpreter corpus.
#
# For every fixtures/build/dexprogs/<name>/classes.dex program this script runs
# `acos run-dex` (release build) and compares stdout byte-for-byte and the exit
# code against fixtures/expected/dexprogs/<name>.{stdout,exit} (jvm-reference).
#
# Usage:
#   tools/diff-dexprogs.sh [--gc-stress]
#
# --gc-stress sets ACOS_GC_STRESS=1 (a collection before every allocation,
# proving GC roots are exact).
#
# Writes results/vm/dexprogs-<mode>.json and exits nonzero unless every program
# passes.
set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
MODE="normal"
STRESS=0
if [ "${1:-}" = "--gc-stress" ]; then
    MODE="gc-stress"
    STRESS=1
fi

BIN="$ROOT/target/release/acos"
FIX="$ROOT/fixtures/build/dexprogs"
EXPECTED="$ROOT/fixtures/expected/dexprogs"
OUTDIR="$ROOT/results/vm"
mkdir -p "$OUTDIR"

if [ ! -d "$FIX" ]; then
    echo "diff-dexprogs: error: $FIX is absent; run tools/build-fixtures.sh" >&2
    exit 2
fi
if [ ! -x "$BIN" ]; then
    echo "diff-dexprogs: building acos (release)..." >&2
    (cd "$ROOT" && cargo build --release -p acos-cli) || exit 2
fi

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

echo "diff-dexprogs: mode=$MODE"
printf '%-22s %-12s %s\n' "PROGRAM" "STATUS" "DETAIL"
printf '%-22s %-12s %s\n' "----------------------" "------------" "------------------------------"

for dir in "$FIX"/*/; do
    name="$(basename "$dir")"
    exp_stdout="$EXPECTED/$name.stdout"
    exp_exit="$(cat "$EXPECTED/$name.exit" 2>/dev/null || echo 0)"
    if [ "$STRESS" = "1" ]; then
        ACOS_GC_STRESS=1 "$BIN" run-dex "$dir/classes.dex" --main Main --json \
            >"$TMP/$name.json" 2>"$TMP/$name.err"
    else
        "$BIN" run-dex "$dir/classes.dex" --main Main --json \
            >"$TMP/$name.json" 2>"$TMP/$name.err"
    fi
    printf '%s\t%s\t%s\n' "$name" "$exp_stdout" "$exp_exit" >>"$TMP/index.tsv"
done

python3 - "$TMP" "$OUTDIR" "$MODE" "$STRESS" <<'PY'
import json, os, sys

tmp, outdir, mode, stress = sys.argv[1], sys.argv[2], sys.argv[3], sys.argv[4] == "1"

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
    name, exp_stdout_path, exp_exit_s = line.rstrip("\n").split("\t")
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
    if got == want and exit_code == exp_exit:
        counts["pass"] += 1
        records.append({"program": name, "status": "pass",
                        "exit_code": exit_code, "metrics": env.get("metrics")})
        print(f"{name:<22} {'PASS':<12} exit={exit_code} stdout={len(got)} bytes")
    else:
        counts["fail"] += 1
        if exit_code != exp_exit:
            detail = f"exit {exit_code} (want {exp_exit})"
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
report = {"mode": mode, "gc_stress": stress, "total": total,
          "pass": counts["pass"], "fail": counts["fail"],
          "unsupported": counts["unsupported"], "programs": records}
path = os.path.join(outdir, f"dexprogs-{mode}.json")
with open(path, "w") as f:
    json.dump(report, f, indent=2)
print()
print(f"dexprogs: {counts['pass']} pass / {counts['fail']} fail / "
      f"{counts['unsupported']} unsupported of {total}")
print(f"dexprogs: report -> {path}")
sys.exit(0 if counts["pass"] == total else 1)
PY
