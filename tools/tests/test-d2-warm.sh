#!/usr/bin/env bash
# test-d2-warm.sh -- tests for the W57 warm D2 harness (tools/art-runtime/d2_warm.py
# and bench-d2.sh --mode warm) that do NOT start an ART guest.
#
# What is asserted (teeth in both directions):
#   A. the analysis self-test is green: benchlib.verdict() decides PASS / TIE /
#      FAIL / NOISY / NOT_MEASURED / NOT_COMPARABLE, a tie is not a pass, a
#      contaminated rep has no samples, and a state or settle mismatch is
#      NOT_COMPARABLE (never a win);
#   B. a synthetic warm outdir aggregates into five rows with the expected
#      verdicts (M1 PASS, M2 TIE, M5 FAIL, M6 PASS, M7 PASS) and the counts
#      match the rows;
#   C. a noisy M1 (alternating totals) is NOISY, and a fully contaminated run
#      is NOT_MEASURED -- neither can pass;
#   D. `bench-d2.sh --mode warm --analyze-warm DIR` re-derives the table with no
#      release binary, no build and no host gate (the re-derive path is not a run);
#   E. the harness refuses a non-release binary path and a missing --acos.
#
# Usage: tools/tests/test-d2-warm.sh
# Scratch: .cache/tests/d2-warm-<pid>/ (removed on success, kept on failure).
set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT"
TMP="$ROOT/.cache/tests/d2-warm-$$"
rm -rf "$TMP"
mkdir -p "$TMP/out"

PASSES=0
FAILURES=0
check() { # check NAME CONDITION DETAIL
    if [ "$2" = "1" ]; then
        PASSES=$((PASSES + 1)); echo "PASS $1"
    else
        FAILURES=$((FAILURES + 1)); echo "FAIL $1${3:+ -- $3}"
    fi
}

DRIVER="$ROOT/tools/art-runtime/d2_warm.py"
FAILED_TMP=""

# ---- A. analysis self-test -------------------------------------------------
OUT="$(python3 -I "$DRIVER" --self-test 2>&1)"; RC=$?
INNER_FAIL="$(printf '%s\n' "$OUT" | grep -c '^FAIL' || true)"
INNER_PASS="$(printf '%s\n' "$OUT" | grep -c '^PASS' || true)"
check "self-test exits 0" "$([ "$RC" = 0 ] && echo 1 || echo 0)" "rc=$RC"
check "self-test has no failing check" "$([ "$INNER_FAIL" = 0 ] && echo 1 || echo 0)" "$OUT"
check "self-test ran at least 10 checks" "$([ "$INNER_PASS" -ge 10 ] && echo 1 || echo 0)" \
    "inner passes=$INNER_PASS"

# ---- synthetic warm reps ---------------------------------------------------
synth() { # synth N TOTAL INCREMENTAL [ALT]
    python3 -I - "$TMP/out" "$1" "$2" "$3" "${4:-0}" <<'PY'
import json, sys
out, n, total, inc, alt = sys.argv[1], int(sys.argv[2]), float(sys.argv[3]), float(sys.argv[4]), sys.argv[5] == "1"
for rep in range(1, 6):
    t = total if not alt else (100.0 if rep % 2 else 400.0)
    doc = {"schema": "acos-bench-d2-warm-rep/1", "phase": "mem", "ok": True,
           "n": n, "rep": rep, "settle_s": 60, "contaminated": False,
           "bare": {"settle_s": 10.0, "net_anon_shmem_mib": 10.0},
           "total_anon_shmem_mib": t,
           "incremental_mib_per_session": inc,
           "per_session": [{"i": 0, "process_anon_shmem_mib": 100.0}],
           "sessions": [{"i": 0, "ok": True}]}
    with open("%s/mem-n%d-rep%d.json" % (out, n, rep), "w") as f:
        json.dump(doc, f)
PY
}
synth 1 150.0 140.0
synth 5 410.0 80.0

python3 -I - "$TMP/out" 100.0 250.0 700.0 <<'PY'
import json, sys
out, m5, m7, m6 = sys.argv[1], float(sys.argv[2]), float(sys.argv[3]), float(sys.argv[4])
lat = {"phase": "latency", "n": 1, "contaminated": False,
       "m5": [{"ms": m5 + 0.1 * i, "good": True, "png_bytes": 9545882} for i in range(20)],
       "m7": [{"ms": m7 + 0.1 * i, "good": True, "tree_bytes": 4101, "text_nodes": 4}
              for i in range(20)],
       "m6": [{"ms": m6 + 0.1 * i, "good": True} for i in range(10)]}
with open(out + "/latency-n1-rep1.json", "w") as f:
    json.dump(lat, f)
PY

# ---- B. aggregate ----------------------------------------------------------
J1="$TMP/result1.json"
M1="$TMP/result1.md"
OUT="$(python3 -I "$DRIVER" --analyze "$TMP/out" --out "$J1" --markdown "$M1" 2>&1)"; RC=$?
check "analyze exits 0" "$([ "$RC" = 0 ] && echo 1 || echo 0)" "$OUT"
check "result JSON written" "$([ -s "$J1" ] && echo 1 || echo 0)"
check "result markdown written" "$([ -s "$M1" ] && echo 1 || echo 0)"
V="$(python3 -I - "$J1" <<'PY'
import json, sys
d = json.load(open(sys.argv[1]))
rows = {r["id"]: r for r in d["rows"]}
out = {
    "rows": len(d["rows"]) == 5,
    "ids": sorted(rows) == ["M1", "M2", "M5", "M6", "M7"],
    "m1": rows["M1"]["verdict"] == "PASS" and rows["M1"]["pass"] is True,
    "m2": rows["M2"]["verdict"] == "TIE" and rows["M2"]["pass"] is False,
    "m5": rows["M5"]["verdict"] == "FAIL" and rows["M5"]["pass"] is False,
    "m6": rows["M6"]["verdict"] == "PASS",
    "m7": rows["M7"]["verdict"] == "PASS",
    "counts": (d["verdict_counts"].get("PASS") == 3
               and d["verdict_counts"].get("TIE") == 1
               and d["verdict_counts"].get("FAIL") == 1),
    "n1": len([r for r in d["reps"] if r.get("n") == 1]) == 5,
    "n5": len([r for r in d["reps"] if r.get("n") == 5]) == 5,
    "marginal": (rows["M2"].get("marginal") or {}).get("marginal_mib_per_session") == 65.0,
    "warm": all(r["acos"]["state"] == "warm" for r in d["rows"]),
}
bad = [k for k, v in out.items() if not v]
print(" ".join(bad))
sys.exit(0 if not bad else 1)
PY
)"; RC=$?
check "row verdicts, counts, reps and marginal are as expected" \
    "$([ "$RC" = 0 ] && echo 1 || echo 0)" "mismatch: $V"
check "markdown carries the verdict counts" \
    "$(grep -q 'Verdict counts' "$M1" && echo 1 || echo 0)"

# ---- C. noisy and contaminated rows cannot pass ----------------------------
rm -rf "$TMP/noisy"; mkdir -p "$TMP/noisy"
synth_noisy() { # same shape, into $TMP/noisy
    python3 -I - "$TMP/noisy" <<'PY'
import json, sys
out = sys.argv[1]
for rep in range(1, 6):
    total = 100.0 if rep % 2 else 400.0
    with open("%s/mem-n1-rep%d.json" % (out, rep), "w") as f:
        json.dump({"ok": True, "n": 1, "rep": rep, "contaminated": False,
                   "bare": {"net_anon_shmem_mib": 10.0},
                   "total_anon_shmem_mib": total,
                   "incremental_mib_per_session": None, "per_session": [],
                   "sessions": []}, f)
PY
}
synth_noisy
python3 -I "$DRIVER" --analyze "$TMP/noisy" --out "$TMP/noisy.json" \
    --markdown "$TMP/noisy.md" >/dev/null 2>&1
NOISY="$(python3 -I - "$TMP/noisy.json" <<'PY'
import json, sys
d = json.load(open(sys.argv[1]))
m1 = [r for r in d["rows"] if r["id"] == "M1"][0]
print("%s/%s" % (m1["verdict"], m1["pass"]))
PY
)"
check "noisy M1 is NOISY and not a pass" \
    "$([ "$NOISY" = "NOISY/False" ] && echo 1 || echo 0)" "$NOISY"

rm -rf "$TMP/cont"; mkdir -p "$TMP/cont"
python3 -I - "$TMP/cont" <<'PY'
import json, sys
out = sys.argv[1]
for rep in range(1, 6):
    with open("%s/mem-n1-rep%d.json" % (out, rep), "w") as f:
        json.dump({"ok": True, "n": 1, "rep": rep, "contaminated": True,
                   "bare": {"net_anon_shmem_mib": 10.0},
                   "total_anon_shmem_mib": 150.0,
                   "incremental_mib_per_session": None, "per_session": [],
                   "sessions": []}, f)
PY
python3 -I "$DRIVER" --analyze "$TMP/cont" --out "$TMP/cont.json" \
    --markdown "$TMP/cont.md" >/dev/null 2>&1
CONT="$(python3 -I - "$TMP/cont.json" <<'PY'
import json, sys
d = json.load(open(sys.argv[1]))
m1 = [r for r in d["rows"] if r["id"] == "M1"][0]
print("%s/%s/%s" % (m1["verdict"], m1["pass"], m1["contaminated_samples"]))
PY
)"
check "fully contaminated M1 is NOT_MEASURED with 5 contaminated reps" \
    "$([ "$CONT" = "NOT_MEASURED/False/5" ] && echo 1 || echo 0)" "$CONT"

# ---- D. bench-d2.sh re-derive path (no binary, no build, no host gate) -----
J2="$TMP/result2.json"
OUT="$(WARM_ANALYZE_OUT="$J2" WARM_ANALYZE_MD="$TMP/result2.md" \
    "$ROOT/tools/art-runtime/bench-d2.sh" --mode warm --analyze-warm "$TMP/out" \
    --out "$TMP/benchd2" 2>&1)"; RC=$?
check "bench-d2.sh --analyze-warm exits 0 without a release binary" \
    "$([ "$RC" = 0 ] && echo 1 || echo 0)" "$OUT"
check "bench-d2.sh --analyze-warm wrote the requested result JSON" \
    "$([ -s "$J2" ] && echo 1 || echo 0)" "$J2"

# ---- E. refusals -----------------------------------------------------------
OUT="$(python3 -I "$DRIVER" --acos /usr/bin/true --phase mem --outdir "$TMP/bad" 2>&1)"; RC=$?
check "non-release binary path is refused (exit 2)" "$([ "$RC" = 2 ] && echo 1 || echo 0)" \
    "rc=$RC $OUT"
OUT="$(python3 -I "$DRIVER" --phase mem --outdir "$TMP/bad" 2>&1)"; RC=$?
check "missing --acos is a usage error (exit 2)" "$([ "$RC" = 2 ] && echo 1 || echo 0)" "rc=$RC"
OUT="$("$ROOT/tools/art-runtime/bench-d2.sh" --mode sideways 2>&1)"; RC=$?
check "bench-d2.sh refuses an unknown --mode (exit 2)" "$([ "$RC" = 2 ] && echo 1 || echo 0)" \
    "rc=$RC $OUT"

echo
echo "test-d2-warm: $PASSES passed, $FAILURES failed"
if [ "$FAILURES" -ne 0 ]; then
    echo "test-d2-warm: scratch kept at $TMP" >&2
    exit 1
fi
rm -rf "$TMP"
exit 0
