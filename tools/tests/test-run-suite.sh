#!/usr/bin/env bash
# test-run-suite.sh -- integration tests for tools/run-suite.sh (W45).
#
# Runs the real runner (real guarded.sh scope, real benchlib sample-scope
# sampler) against a fake `acos` binary that writes suit-shaped artifacts, so the
# measurement/gating logic is exercised without starting an ART guest.
#
# What is asserted (teeth in both directions):
#   A. a clean T01 run is accepted; M11 uses the PRIMARY anon+shmem denominator
#      only; every other denominator is recorded with gate_uses=false; the guest
#      scope column is present; the two variant runs own two DISTINCT scopes;
#      passing artifacts are pruned, failing artifacts survive.
#   B. --keep-artifacts keeps everything.
#   C. a wrong twin that PASSES is a false pass: exit 1 and M11 := 0.
#   D. a sampler that reports one job cgroup for both variants (the pre-W45
#      suite-lifetime scope) is rejected: exit 1, scope_check.ok=false,
#      verdict_vs_bar=INVALID_SCOPE.
#
# Usage: tools/tests/test-run-suite.sh
# Scratch: .cache/tests/run-suite-<pid>/ (removed on success, kept on failure).
set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
TMP="$ROOT/.cache/tests/run-suite-$$"
rm -rf "$TMP"
mkdir -p "$TMP/bin" "$TMP/art"

PASSES=0
FAILURES=0

check() { # check NAME CONDITION DETAIL
    if [ "$2" = "1" ]; then
        PASSES=$((PASSES + 1)); echo "PASS $1"
    else
        FAILURES=$((FAILURES + 1)); echo "FAIL $1${3:+ -- $3}"
    fi
}

py() { # count the inner PASS/FAIL lines so the suite totals are honest
    local out rc
    out=$(python3 -I - "$@")
    rc=$?
    printf '%s\n' "$out"
    PASSES=$((PASSES + $(printf '%s\n' "$out" | grep -c '^PASS' || true)))
    FAILURES=$((FAILURES + $(printf '%s\n' "$out" | grep -c '^FAIL' || true)))
    if [ "$rc" != "0" ] && ! printf '%s\n' "$out" | grep -q '^FAIL'; then
        FAILURES=$((FAILURES + 1)); echo "FAIL inner check crashed (rc=$rc)"
    fi
}

# --------------------------------------------------------------------------- #
# fake acos: writes verdict.json / engine.json / a 1 MiB frame, holds ~32 MiB
# --------------------------------------------------------------------------- #
cat >"$TMP/bin/acos" <<'EOS'
#!/usr/bin/env python3
import json, os, sys, time
args = sys.argv[1:]
art = scen = None
i = 0
while i < len(args):
    a = args[i]
    if a == "--artifacts":
        art = args[i + 1]; i += 2; continue
    if a == "--engine":
        i += 2; continue
    if a in ("--json", "verify"):
        i += 1; continue
    scen = a; i += 1
if not art:
    print("fake acos: no --artifacts", file=sys.stderr); sys.exit(2)
os.makedirs(os.path.join(art, "art-frames"), exist_ok=True)
buf = bytearray(32 << 20)           # touch 32 MiB so the sampler sees anon
for off in range(0, len(buf), 4096):
    buf[off] = 1
wrong = "wrong" in os.path.basename(scen or "")
pass_wrong = os.environ.get("FAKE_ACOS_PASS_WRONG") == "1"
verdict = "fail" if (wrong and not pass_wrong) else "pass"
steps = [{"step": "launch", "status": "pass", "duration_ms": 20, "index": 0}]
if verdict == "pass":
    steps.append({"step": "assert_text", "status": "pass", "duration_ms": 10, "index": 1})
else:
    steps.append({"step": "assert_text", "status": "fail", "duration_ms": 10,
                  "index": 1, "error": {"code": "ASSERTION_FAILED",
                                        "message": "seeded wrong twin"}})
with open(os.path.join(art, "verdict.json"), "w") as f:
    json.dump({"verdict": verdict, "steps": steps,
               "resources": {"wall_ms": 40}}, f)
with open(os.path.join(art, "engine.json"), "w") as f:
    json.dump({"engine": {"timing": {"session_start_ms": 10, "shutdown_ms": 5}}}, f)
with open(os.path.join(art, "art-frames", "session-timing.json"), "w") as f:
    json.dump({"session_start_ms": 10, "shutdown_ms": 5}, f)
with open(os.path.join(art, "tree.json"), "w") as f:
    json.dump({"nodes": [{"text": "0"}]}, f)
with open(os.path.join(art, "frame0.png"), "wb") as f:
    f.write(b"p" * (1 << 20))
time.sleep(0.2)
del buf
sys.exit(0 if verdict == "pass" else 1)
EOS
chmod +x "$TMP/bin/acos"

# --------------------------------------------------------------------------- #
# fake sampler: always reports ONE job cgroup (scope-reuse defect, case D)
# --------------------------------------------------------------------------- #
cat >"$TMP/bin/reused-sampler.py" <<'EOS'
import json, os, subprocess, sys, time
args = sys.argv[1:]
assert args and args[0] == "sample-scope", args
out = None
cmd = []
i = 1
while i < len(args):
    a = args[i]
    if a == "--":
        cmd = args[i + 1:]
        break
    if a in ("--out", "--interval", "--label", "--info-dir"):
        if a == "--out":
            out = args[i + 1]
        i += 2
        continue
    i += 1
t0 = time.time()
rc = subprocess.call(cmd)
dur = max(0.001, time.time() - t0)
den = {
    "primary": {"gib_min": 0.02, "gib_min_excl_sampler": 0.02, "gate_uses": True},
    "resident": {"gib_min": 0.021, "gib_min_excl_sampler": 0.021, "gate_uses": False},
    "job_all_incl_file": {"gib_min": 0.06, "gib_min_excl_sampler": 0.06,
                          "gate_uses": False},
    "scope_runner_formula": {"gib_min": 0.01, "gib_min_excl_sampler": 0.01,
                             "gate_uses": False},
    "infra_file_cache": {"gib_min": 0.04, "charged_to_task": False},
    "sampler": {"overhead_gib_min": 0.0001, "rss_mib": 20.0},
    "components": {"peak_mib_all_cgroups": 120.0, "peak_mib_primary": 110.0,
                   "anon_gib_min": 0.015, "shmem_gib_min": 0.005,
                   "kernel_gib_min": 0.001, "sock_gib_min": 0.0,
                   "file_incl_shmem_gib_min": 0.04, "file_excl_shmem_gib_min": 0.04},
    "scope": {"job_cgroup": "/sys/fs/cgroup/fake/reused.scope",
              "guest_cgroups": ["/sys/fs/cgroup/fake/guest.scope"],
              "scope_count": 2, "closing_sample": True},
}
data = {"schema": "acos-scope-sample/1", "rc": rc, "wall_s": dur,
        "scope_available": True, "scope_limit": True,
        "job_cgroup": "/sys/fs/cgroup/fake/reused.scope", "sampler_pid": 4242,
        "samples": {"ticks": 3, "job": 2, "misses": 0, "pss_ticks": 1,
                    "interval_s": 0.1},
        "denominators": den}
with open(out, "w") as f:
    json.dump(data, f)
sys.exit(rc)
EOS

run_suite() { # run_suite OUT [extra args...]
    local out="$1"; shift
    bash "$ROOT/tools/run-suite.sh" --only "${ONLY:-T01}" --rounds 1 \
        --acos "$TMP/bin/acos" --acos-art /bin/true \
        --artifacts "$TMP/art" --out "$out" --no-host-gate --mem 2G \
        --timeout 120 "$@"
}

# --------------------------------------------------------------------------- #
# A. clean run
# --------------------------------------------------------------------------- #
echo "--- case A: clean run (T01,T02 for a 2-task bootstrap CI)"
ONLY="T01,T02" run_suite "$TMP/a.json" >"$TMP/a.log" 2>&1
RC_A=$?
check "A exit 0" "$([ "$RC_A" = "0" ] && echo 1 || echo 0)" "rc=$RC_A"
py "$TMP/a.json" <<'PY'
import json, os, sys
d = json.load(open(sys.argv[1]))
art = os.path.dirname(sys.argv[1])
def ck(name, cond, detail=""):
    print(("PASS " if cond else "FAIL ") + name + ("" if cond else " -- " + str(detail)))
m = d["m11"]; s = d["summary"]
ck("A verified 2/2", s["verified_tasks"] == 2 and s["tasks_expressible"] == 2, s)
ck("A false_pass 0", s["false_pass_count"] == 0, s["false_pass_count"])
ck("A primary denominator gates", m["denominators"]["primary"]["gate_uses"] is True)
for k in ("resident", "job_all_incl_file", "scope_runner_formula"):
    ck("A %s is not a gate value" % k, m["denominators"][k]["gate_uses"] is False)
ck("A primary gib_min > 0", (m["gib_min"] or 0) > 0, m["gib_min"])
ck("A other denominators recorded",
   all(m["denominators"][k]["m11"]["gib_min"] is not None
       for k in ("resident", "job_all_incl_file", "scope_runner_formula")),
   {k: m["denominators"][k]["m11"]["gib_min"] for k in m["denominators"]})
ck("A infra file cache reported separately",
   m["infra_file_cache_gib_min"] is not None, m["infra_file_cache_gib_min"])
ck("A M11 CI present", m["ci95_low"] is not None and m["ci95_high"] is not None, m)
ck("A verdict vs bar computed", m["verdict_vs_bar"] in ("PASS", "FAIL"),
   m["verdict_vs_bar"])
ck("A fresh scope per variant", d["measurement"]["fresh_scope_per_variant"] is True)
ck("A scope check ok", m["scope_check"]["ok"] is True, m["scope_check"])
ck("A four distinct scopes for four variant runs",
   m["scope_check"]["distinct_scopes"] == 4 and m["scope_check"]["variants"] == 4,
   m["scope_check"])
scopes = [r["mem"]["job_cgroup"] for t in d["tasks"] for k in ("good", "wrong")
          for r in (t.get(k) or {}).get("runs") or []]
ck("A job cgroups are real guarded scopes",
   all(s and ".scope" in s for s in scopes), scopes)
ck("A job cgroups differ", len(set(scopes)) == len(scopes), scopes)
mems = [r["mem"] for t in d["tasks"] for k in ("good", "wrong")
        for r in (t.get(k) or {}).get("runs") or []]
ck("A guest scope column present (may be 0 with the fake app)",
   all("guest_cgroups" in x and isinstance(x["guest_cgroups"], list) for x in mems),
   mems)
ck("A sampler excluded with evidence",
   all(x["sampler_overhead_gib_min"] is not None for x in mems), mems)
good = d["tasks"][0]["good"]["runs"][0]["artifacts"]
wrong = d["tasks"][0]["wrong"]["runs"][0]["artifacts"]
ck("A passing frame pruned", not os.path.exists(os.path.join(good, "frame0.png")))
ck("A passing verdict kept", os.path.exists(os.path.join(good, "verdict.json")))
ck("A passing step timing kept",
   os.path.exists(os.path.join(good, "art-frames", "session-timing.json")))
ck("A failing frame kept (failure evidence)",
   os.path.exists(os.path.join(wrong, "frame0.png")))
ck("A prune reported", d["artifacts"]["pruned"] is True and
   d["artifacts"]["freed_bytes"] > 0, d["artifacts"]["freed_bytes"])
PY

# --------------------------------------------------------------------------- #
# B. --keep-artifacts
# --------------------------------------------------------------------------- #
echo "--- case B: --keep-artifacts"
run_suite "$TMP/b.json" --keep-artifacts >"$TMP/b.log" 2>&1
RC_B=$?
check "B exit 0" "$([ "$RC_B" = "0" ] && echo 1 || echo 0)" "rc=$RC_B"
py "$TMP/b.json" <<'PY'
import json, os, sys
d = json.load(open(sys.argv[1]))
good = d["tasks"][0]["good"]["runs"][0]["artifacts"]
print(("PASS " if os.path.exists(os.path.join(good, "frame0.png")) else "FAIL ")
      + "B --keep-artifacts keeps the frame")
print(("PASS " if d["artifacts"]["kept_all"] and not d["artifacts"]["pruned"] else "FAIL ")
      + "B prune disabled")
PY

# --------------------------------------------------------------------------- #
# C. false pass (wrong twin passes) -> exit 1, M11 := 0
# --------------------------------------------------------------------------- #
echo "--- case C: false pass"
FAKE_ACOS_PASS_WRONG=1 run_suite "$TMP/c.json" >"$TMP/c.log" 2>&1
RC_C=$?
check "C exit 1 (not accepted)" "$([ "$RC_C" = "1" ] && echo 1 || echo 0)" "rc=$RC_C"
py "$TMP/c.json" <<'PY'
import json, sys
d = json.load(open(sys.argv[1]))
m = d["m11"]; s = d["summary"]
def ck(n, c, x=""):
    print(("PASS " if c else "FAIL ") + n + ("" if c else " -- " + str(x)))
ck("C false_pass counted", s["false_pass_count"] == 1, s["false_pass_count"])
ck("C false pass zeroes M11", m["m11_tasks_per_gib_min"] == 0.0,
   m["m11_tasks_per_gib_min"])
ck("C false pass is not above the bar", m["above_legacy_bar"] is False)
ck("C false pass verdict FAIL", m["verdict_vs_bar"] == "FAIL", m["verdict_vs_bar"])
PY

# --------------------------------------------------------------------------- #
# D. scope reuse (suite-lifetime scope) -> exit 1, INVALID_SCOPE
# --------------------------------------------------------------------------- #
echo "--- case D: scope reuse rejected"
run_suite "$TMP/d.json" --sampler "$TMP/bin/reused-sampler.py" >"$TMP/d.log" 2>&1
RC_D=$?
check "D exit 1 (measurement defect)" "$([ "$RC_D" = "1" ] && echo 1 || echo 0)" "rc=$RC_D"
py "$TMP/d.json" <<'PY'
import json, sys
d = json.load(open(sys.argv[1]))
m = d["m11"]
def ck(n, c, x=""):
    print(("PASS " if c else "FAIL ") + n + ("" if c else " -- " + str(x)))
ck("D scope check fails", m["scope_check"]["ok"] is False, m["scope_check"])
ck("D reused scope named", any("reused" in e for e in m["scope_check"]["errors"]),
   m["scope_check"]["errors"])
ck("D one distinct scope only", m["scope_check"]["distinct_scopes"] == 1,
   m["scope_check"])
ck("D verdict is INVALID_SCOPE", m["verdict_vs_bar"] == "INVALID_SCOPE",
   m["verdict_vs_bar"])
ck("D not gate eligible as a pass", m["above_legacy_bar"] is False)
PY
grep -q "SCOPE DEFECT" "$TMP/d.log" && echo "PASS D stderr names the scope defect" \
    && PASSES=$((PASSES + 1)) || { echo "FAIL D stderr names the scope defect"; FAILURES=$((FAILURES + 1)); }

echo
echo "$PASSES passed, $FAILURES failed (scratch $TMP)"
if [ "$FAILURES" = "0" ]; then
    rm -rf "$TMP"
    exit 0
fi
exit 1
