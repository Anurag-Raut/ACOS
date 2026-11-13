#!/usr/bin/env python3
"""Tests for tools/benchlib.py. Plain Python, no third-party dependencies.

Run:  python3 tools/tests/test-benchlib.py
      BENCHLIB_PATH=/path/to/benchlib.py python3 tools/tests/test-benchlib.py

The suite has teeth in two directions:

1. every rule is asserted positively (a clear win passes, a tie does not, ...);
2. the negative cases are asserted explicitly -- a row with a 22 % spread must
   be NOISY and must never PASS, and a matrix with a false pass must not
   validate. To prove those tests can fail, run the same file against a
   mutated library (the W29 evidence does this):

     sed 's/return spread > max_spread/return False/' tools/benchlib.py \
         > .cache/benchlib-mutant.py
     BENCHLIB_PATH=.cache/benchlib-mutant.py python3 tools/tests/test-benchlib.py
     # expected: FAIL test_noisy_row_cannot_pass (mutation is caught)

W45 denominator teeth (each mutant below is caught; W45.md records the output):

     # guest sibling cgroup dropped from the sum (W41 defect 1.1)
     sed 's/for cg, rows in (guest_series or {}).items():/for cg, rows in {}.items():/' \
         tools/benchlib.py > .cache/m-no-guest.py
     # shared page cache charged to the task (W41 defect 1.2)
     sed 's/^PRIMARY_KEYS = ("anon", "shmem")/PRIMARY_KEYS = ("anon", "shmem", "file")/' \
         tools/benchlib.py > .cache/m-file.py
     # suite-lifetime scope accepted (W41 defect 1.3)
     sed 's/        if len(names) > 1:/        if False:/' \
         tools/benchlib.py > .cache/m-scope.py
     # artifacts never pruned (decision e)
     sed 's/^    if keep:$/    if True:/' tools/benchlib.py > .cache/m-noprune.py
     for m in .cache/m-*.py; do
       BENCHLIB_PATH=$m python3 tools/tests/test-benchlib.py | grep -E '^FAIL|passed,'
     done
"""

import importlib.util
import json
import os
import sys
import tempfile
import time
from pathlib import Path

HERE = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.dirname(os.path.dirname(HERE))
DEFAULT_LIB = os.path.join(REPO, "tools", "benchlib.py")
LIB_PATH = os.environ.get("BENCHLIB_PATH", DEFAULT_LIB)

_spec = importlib.util.spec_from_file_location("benchlib", LIB_PATH)
bl = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(bl)

PASSES = 0
FAILURES = 0


def check(name, cond, detail=""):
    global PASSES, FAILURES
    if cond:
        PASSES += 1
        print("PASS %s" % name)
    else:
        FAILURES += 1
        print("FAIL %s%s" % (name, (" -- " + detail) if detail else ""))


def eq(name, got, want):
    check(name, got == want, "got %r want %r" % (got, want))


def close(name, got, want, tol=1e-9):
    ok = got is not None and abs(float(got) - float(want)) <= tol
    check(name, ok, "got %r want %r (+-%g)" % (got, want, tol))


# --------------------------------------------------------------------------- #
# statistics
# --------------------------------------------------------------------------- #

def test_percentile():
    xs = list(range(1, 101))  # 1..100
    close("percentile p50 of 1..100", bl.percentile(xs, 50), 50.5)
    close("percentile p5 of 1..100", bl.percentile(xs, 5), 5.95)
    close("percentile p95 of 1..100", bl.percentile(xs, 95), 95.05)
    close("percentile p0 is min", bl.percentile([7, 3, 9], 0), 3)
    close("percentile p100 is max", bl.percentile([7, 3, 9], 100), 9)
    eq("percentile empty is None", bl.percentile([], 50), None)
    close("percentile single sample", bl.percentile([4.0], 95), 4.0)


def test_stats_basics():
    st = bl.stats([1, 2, 3, 4, 5], resamples=200)
    eq("stats n", st["n"], 5)
    close("stats mean", st["mean"], 3.0)
    close("stats median", st["median"], 3.0)
    close("stats min", st["min"], 1.0)
    close("stats max", st["max"], 5.0)
    check("stats spread positive", st["spread"] is not None and st["spread"] > 0)
    check("stats bootstrap CI produced", st["ci95_low"] is not None and st["ci95_high"] is not None)
    check("stats CI brackets p50",
          st["ci95_low"] <= st["median"] <= st["ci95_high"],
          "%r .. %r around %r" % (st["ci95_low"], st["ci95_high"], st["median"]))

    empty = bl.stats([])
    eq("stats empty n", empty["n"], 0)
    eq("stats empty p50", empty["p50"], None)
    check("stats empty is noisy (unmeasurable)", bl.is_noisy(empty))

    one = bl.stats([42.0])
    eq("stats single p50", one["p50"], 42.0)
    eq("stats single spread", one["spread"], None)
    check("stats single sample is not stable", bl.is_noisy(one))

    # None/NaN are dropped, not turned into zeroes.
    st2 = bl.stats([1.0, None, float("nan"), 3.0])
    eq("stats drops None/NaN", st2["n"], 2)


def test_stats_reproducible_bootstrap():
    xs = [10, 12, 11, 13, 12, 11, 14, 10, 12, 13]
    a = bl.stats(xs, resamples=500, seed=7)
    b = bl.stats(xs, resamples=500, seed=7)
    eq("bootstrap reproducible (ci_low)", a["ci95_low"], b["ci95_low"])
    eq("bootstrap reproducible (ci_high)", a["ci95_high"], b["ci95_high"])


def test_spread_gate():
    # spread = (p95-p5)/p50. A tidy row well below the 15% gate:
    tidy = bl.stats([100, 101, 100, 102, 99, 100, 101, 100, 102, 100])
    check("tidy row spread below gate", tidy["spread"] < bl.MAX_SPREAD,
          "spread=%r" % tidy["spread"])
    check("tidy row not noisy", not bl.is_noisy(tidy))

    # A deliberately noisy row: 22% spread, the M5 failure mode.
    noisy = bl.stats([80, 100, 100, 100, 100, 100, 100, 100, 120],
                     resamples=100)
    check("noisy row spread > 15%", noisy["spread"] > 0.15,
          "spread=%r" % noisy["spread"])
    check("noisy row is noisy", bl.is_noisy(noisy))


def test_noise_floor():
    tidy = bl.stats([100] * 10 + [101])
    nf = bl.noise_floor(tidy, tidy, min_noise=0.05)
    check("noise floor never below 5%", nf["noise"] >= 0.05, repr(nf))
    spread_row = bl.stats([90, 100, 110] * 4)
    nf2 = bl.noise_floor(spread_row, tidy)
    close("noise floor is max spread", nf2["noise"], spread_row["spread"], 1e-12)


# --------------------------------------------------------------------------- #
# verdict (D2 rule)
# --------------------------------------------------------------------------- #

def _stable(x, n=25):
    return [x * (1.0 + (0.005 if i % 2 else -0.005)) for i in range(n)]


def test_verdict_pass_clear_win():
    legacy = bl.stats(_stable(100.0))
    acos = bl.stats(_stable(70.0))
    v = bl.verdict(acos, legacy, metric_type="PSS", state="warm")
    eq("clear win verdict", v["verdict"], "PASS")
    check("clear win is a pass", v["pass"])
    check("clear win ratio ~0.7", abs(v["ratio"] - 0.7) < 0.02, repr(v["ratio"]))


def test_verdict_tie_is_not_pass():
    legacy = bl.stats(_stable(100.0))
    acos = bl.stats(_stable(100.5))
    v = bl.verdict(acos, legacy, metric_type="PSS", state="warm")
    eq("tie verdict", v["verdict"], "TIE")
    check("tie is NOT a pass", not v["pass"])
    eq("tie threshold recorded", v["threshold"] is not None, True)


def test_verdict_loss_is_fail():
    legacy = bl.stats(_stable(100.0))
    acos = bl.stats(_stable(140.0))
    v = bl.verdict(acos, legacy, metric_type="PSS", state="warm")
    eq("loss verdict", v["verdict"], "FAIL")
    check("loss is not a pass", not v["pass"])


def test_verdict_missing_is_not_measured():
    legacy = bl.stats(_stable(100.0))
    v = bl.verdict([], legacy, metric_type="PSS", state="warm")
    eq("missing acos verdict", v["verdict"], "NOT_MEASURED")
    check("missing is not a pass", not v["pass"])
    v2 = bl.verdict(legacy, None, metric_type="PSS", state="warm")
    eq("missing legacy verdict", v2["verdict"], "NOT_MEASURED")


def test_verdict_type_mismatch_not_comparable():
    legacy = bl.stats(_stable(100.0))
    acos = bl.stats(_stable(50.0))
    v = bl.verdict(acos, legacy, metric_type="CGROUP_MEMORY_CURRENT",
                   legacy_type="CGROUP_ANON_SHMEM", state="warm")
    eq("type mismatch verdict", v["verdict"], "NOT_COMPARABLE")
    check("type mismatch is not a pass", not v["pass"])
    v2 = bl.verdict(acos, legacy, metric_type="PSS", state="cold-process",
                    legacy_state="warm")
    eq("state mismatch verdict", v2["verdict"], "NOT_COMPARABLE")


def test_verdict_insufficient_samples():
    legacy = bl.stats(_stable(100.0))
    acos = bl.stats([70.0])  # one sample cannot show a distribution
    v = bl.verdict(acos, legacy, metric_type="PSS", state="warm", min_samples=3)
    eq("insufficient verdict", v["verdict"], "INSUFFICIENT")
    check("insufficient is not a pass", not v["pass"])


def test_noisy_row_cannot_pass():
    """TEETH: the M5 failure mode (22% spread) must never be a PASS."""
    legacy = bl.stats(_stable(100.0))
    # ACOS is numerically 25% faster but wildly unstable: p5=78, p95=120.
    acos = bl.stats([78, 95, 100, 105, 120, 80, 118, 100, 101, 99],
                    resamples=200)
    check("synthetic row really is >15% spread", acos["spread"] > 0.15,
          "spread=%r" % acos["spread"])
    v = bl.verdict(acos, legacy, metric_type="PSS", state="warm")
    eq("noisy row verdict", v["verdict"], "NOISY")
    check("noisy row is NOT a pass", not v["pass"])
    check("noisy row reason names the gate",
          any("noisy" in r for r in v["reasons"]), repr(v["reasons"]))


def test_noisy_legacy_row_blocks_pass():
    legacy = bl.stats([78, 95, 100, 105, 120, 80, 118, 100, 101, 99],
                      resamples=200)
    acos = bl.stats(_stable(50.0))
    v = bl.verdict(acos, legacy, metric_type="PSS", state="warm")
    eq("noisy legacy verdict", v["verdict"], "NOISY")
    check("noisy legacy is not a pass", not v["pass"])


def test_verdict_higher_is_better():
    legacy = bl.stats(_stable(24.4))
    acos = bl.stats(_stable(60.0))
    v = bl.verdict(acos, legacy, metric_type="TASKS_PER_GIB_MIN",
                   state="warm", lower_is_better=False)
    eq("throughput win verdict", v["verdict"], "PASS")
    check("throughput win is a pass", v["pass"])


# --------------------------------------------------------------------------- #
# ABBA
# --------------------------------------------------------------------------- #

def test_abba_order_and_stats():
    calls = []

    def a():
        calls.append("A")
        return 10.0

    def b():
        calls.append("B")
        return 20.0

    r = bl.abba(a, b, pairs=3)
    eq("abba call order", calls, ["A", "B", "B", "A"] * 3)
    eq("abba order strings", r["order"], ["ABBA", "ABBA", "ABBA"])
    eq("abba a n", r["a"]["n"], 6)
    eq("abba b n", r["b"]["n"], 6)
    close("abba a p50", r["a"]["p50"], 10.0)
    check("abba 3 pairs is enough", r["pairs_ok"])

    r2 = bl.abba(a, b, pairs=2)
    check("abba 2 pairs flagged insufficient", not r2["pairs_ok"])


def test_abba_alternating_drift():
    calls = []
    r = bl.abba(lambda: calls.append("A"), lambda: calls.append("B"),
                pairs=2, alternate_pairs=True)
    eq("abba alternates pairs", calls, ["A", "B", "B", "A", "B", "A", "A", "B"])
    check("abba reversed pair recorded", r["order"][1] == "BAAB")


# --------------------------------------------------------------------------- #
# host gate
# --------------------------------------------------------------------------- #

def test_host_gate_refuses_low_ram():
    with tempfile.TemporaryDirectory() as d:
        mem = os.path.join(d, "meminfo")
        with open(mem, "w") as f:
            f.write("MemTotal: 64000000 kB\nMemAvailable: 8000000 kB\n")
        load = os.path.join(d, "loadavg")
        with open(load, "w") as f:
            f.write("0.10 0.20 0.30 1/100 1\n")
        g = bl.host_gate(d, meminfo_path=mem, loadavg_path=load,
                         check_lock=False, min_avail_gib=30.0)
        check("low RAM gate refuses", not g["ok"], repr(g["reasons"]))
        check("low RAM reason names MemAvailable",
              any("MemAvailable" in r for r in g["reasons"]))
        close("gate records available GiB", g["mem_available_gib"], 7.63, 0.02)


def test_host_gate_refuses_high_load():
    with tempfile.TemporaryDirectory() as d:
        mem = os.path.join(d, "meminfo")
        with open(mem, "w") as f:
            f.write("MemTotal: 64000000 kB\nMemAvailable: 48000000 kB\n")
        load = os.path.join(d, "loadavg")
        with open(load, "w") as f:
            f.write("9.50 4.00 2.00 2/100 1\n")
        g = bl.host_gate(d, meminfo_path=mem, loadavg_path=load,
                         check_lock=False)
        check("high load gate refuses", not g["ok"], repr(g["reasons"]))
        check("high load reason names loadavg1",
              any("loadavg1" in r for r in g["reasons"]))


def test_host_gate_ok():
    with tempfile.TemporaryDirectory() as d:
        mem = os.path.join(d, "meminfo")
        with open(mem, "w") as f:
            f.write("MemTotal: 64000000 kB\nMemAvailable: 48000000 kB\n")
        load = os.path.join(d, "loadavg")
        with open(load, "w") as f:
            f.write("0.25 0.50 0.70 1/100 1\n")
        g = bl.host_gate(d, meminfo_path=mem, loadavg_path=load,
                         check_lock=False)
        check("clean host passes gate", g["ok"], repr(g["reasons"]))


def test_art_lock_probe():
    with tempfile.TemporaryDirectory() as d:
        lock = os.path.join(d, "art.lock")
        open(lock, "w").close()
        eq("free lock is not held", bl.art_lock_held(lock), False)
        import fcntl
        with open(lock, "a") as f:
            fcntl.flock(f.fileno(), fcntl.LOCK_EX | fcntl.LOCK_NB)
            eq("held lock is detected", bl.art_lock_held(lock), True)
            fcntl.flock(f.fileno(), fcntl.LOCK_UN)
        eq("released lock is free again", bl.art_lock_held(lock), False)


# --------------------------------------------------------------------------- #
# provenance
# --------------------------------------------------------------------------- #

def test_profile_detection():
    eq("release path", bl.detect_profile("/r/target/release/acos"), "release")
    eq("debug path", bl.detect_profile("/r/target/debug/acos"), "debug")
    eq("custom target release", bl.detect_profile("/r/.cache/w29-target/release/acos"), "release")
    eq("unknown path", bl.detect_profile("/usr/bin/acos"), "unknown")


def test_binary_provenance_and_stamp():
    with tempfile.TemporaryDirectory() as d:
        rel = os.path.join(d, "target", "release", "acos")
        dbg = os.path.join(d, "target", "debug", "acos")
        os.makedirs(os.path.dirname(rel))
        os.makedirs(os.path.dirname(dbg))
        for p in (rel, dbg):
            with open(p, "wb") as f:
                f.write(b"fake-binary")
        bp = bl.binary_provenance(rel)
        eq("release binary profile", bp["profile"], "release")
        check("release binary eligible", bp["verdict_eligible"])
        eq("release binary sha256 length", len(bp["sha256"]), 64)

        stamp = bl.provenance(d, [rel], profile="release")
        check("release stamp eligible", stamp["verdict_eligible"],
              repr(stamp["not_verdict_eligible_reason"]))
        eq("stamp profile", stamp["profile"], "release")
        eq("stamp binary count", len(stamp["binaries"]), 1)

        stamp_dbg = bl.provenance(d, [dbg], profile="debug")
        check("debug stamp NOT eligible", not stamp_dbg["verdict_eligible"])
        check("debug stamp names the reason",
              "debug" in (stamp_dbg["not_verdict_eligible_reason"] or ""))


# --------------------------------------------------------------------------- #
# memory: integrals and same-type comparison
# --------------------------------------------------------------------------- #

def test_sample_integral():
    # 1 GiB held for exactly 60 s = 1 GiB-min.
    one_gib = 1024 ** 3
    g = bl.sample_integral([{"t": 0, "bytes": one_gib},
                            {"t": 60, "bytes": one_gib}])
    close("constant 1 GiB for 60 s == 1 GiB-min", g, 1.0, 1e-9)
    # 2 GiB for 30 s = 1 GiB-min.
    g2 = bl.sample_integral([{"t": 100, "bytes": 2 * one_gib},
                             {"t": 130, "bytes": 2 * one_gib}])
    close("constant 2 GiB for 30 s == 1 GiB-min", g2, 1.0, 1e-9)
    # Linear ramp 0 -> 2 GiB over 60 s = 1 GiB-min (trapezoid).
    g3 = bl.sample_integral([{"t": 0, "bytes": 0},
                             {"t": 60, "bytes": 2 * one_gib}])
    close("ramp 0->2 GiB over 60 s", g3, 1.0, 1e-9)
    # Out-of-order rows are sorted, not subtracted.
    g4 = bl.sample_integral([{"t": 60, "bytes": one_gib},
                             {"t": 0, "bytes": one_gib}])
    close("out-of-order rows sorted", g4, 1.0, 1e-9)
    eq("empty samples -> 0", bl.sample_integral([]), 0.0)
    eq("peak of samples", bl.sample_peak([{"bytes": 3}, {"bytes": 9}]), 9.0)


def test_compare_memory_same_type():
    acos = {"value_mib": 155.3, "metric_type": "cgroup_anon_shmem",
            "state": "warm", "settle_s": 60, "n": 5}
    legacy = bl.legacy_memory_reference()
    c = bl.compare_memory(acos, legacy)
    eq("same-type memory verdict", c["verdict"], "COMPARABLE")
    check("same-type is comparable", c["comparable"])
    close("same-type ratio", c["ratio"], 155.3 / 130.3, 1e-9)


def test_compare_memory_type_mismatch():
    # The MILESTONE-1 comparison: cgroup peak + launcher + bwrap vs legacy anon+shmem.
    acos = {"value_mib": 171.4, "metric_type": "CGROUP_MEMORY_CURRENT",
            "state": "warm", "settle_s": None}
    c = bl.compare_memory(acos, bl.legacy_memory_reference())
    eq("mismatched memory types", c["verdict"], "NOT_COMPARABLE")
    check("mismatch is NOT comparable", not c["comparable"])
    check("mismatch names the types",
          any("metric type" in r for r in c["reasons"]), repr(c["reasons"]))


def test_compare_memory_state_and_settle():
    acos = {"value_mib": 155.3, "metric_type": "CGROUP_ANON_SHMEM",
            "state": "cold-process", "settle_s": 60}
    c = bl.compare_memory(acos, bl.legacy_memory_reference())
    eq("cold vs warm NOT_COMPARABLE", c["verdict"], "NOT_COMPARABLE")
    acos2 = {"value_mib": 155.3, "metric_type": "CGROUP_ANON_SHMEM",
             "state": "warm", "settle_s": 5}
    c2 = bl.compare_memory(acos2, bl.legacy_memory_reference())
    eq("settle-time mismatch NOT_COMPARABLE", c2["verdict"], "NOT_COMPARABLE")


# --------------------------------------------------------------------------- #
# failure taxonomy
# --------------------------------------------------------------------------- #

def test_classify_error():
    a = bl.classify_error("ASSERTION_FAILED")
    eq("assertion class", a["class"], "assertion")
    eq("assertion not retryable", a["retryable"], False)
    check("assertion has a hint", bool(a["hint"]))
    eq("element-not-found is assertion", bl.classify_error("ELEMENT_NOT_FOUND")["class"],
       "assertion")
    eq("unsupported class", bl.classify_error("UNSUPPORTED_RN_KEY")["class"], "unsupported")
    eq("timeout class", bl.classify_error("TIMEOUT")["class"], "infra_timeout")
    eq("timeout retryable", bl.classify_error("TIMEOUT")["retryable"], True)
    eq("crash class", bl.classify_error("APP_CRASHED")["class"], "app_crash")
    eq("unknown stays unknown", bl.classify_error("NOT_A_REAL_CODE")["class"], "unknown")
    eq("lowercase normalised", bl.classify_error("timeout")["code"], "TIMEOUT")


# --------------------------------------------------------------------------- #
# matrix schema validation
# --------------------------------------------------------------------------- #

def valid_matrix():
    run = {
        "run_id": "r1",
        "utc": "2026-11-12T00:00:00Z",
        "scenario": "scenarios/negative/missing-element.yaml",
        "scenario_sha256": "0" * 64,
        "apk_sha256": "1" * 64,
        "variant": "negative",
        "expected": "fail",
        "verdict": "fail",
        "false_pass": False,
        "error": bl.classify_error("ELEMENT_NOT_FOUND"),
        "state": "cold-process",
        "n_concurrent": 1,
        "wall_ms": 1234.0,
        "first_frame_ms": 500.0,
        "steps": [],
        "mem": {"cgroup_peak_mib": 120.0, "cgroup_gib_min": 0.01,
                "anon_shmem_settled_mib": None, "pss_mib": 110.0,
                "metric_type": "CGROUP_ANON_SHMEM", "settle_s": 0},
        "host": {"loadavg1": 0.3, "mem_available_mib": 40000.0},
        "profile": "release",
        "engine": "art",
        "verdict_eligible": True,
        "negative": True,
    }
    return {
        "schema": bl.MATRIX_SCHEMA,
        "provenance": {"utc": "2026-11-12T00:00:00Z", "git_head": "abc",
                       "profile": "release", "verdict_eligible": True,
                       "binaries": []},
        "runs": [run],
        "summary": {"total_runs": 1, "passed": 0, "failed": 1, "false_pass": 0,
                    "negative_runs": 1, "unclassified": 0},
    }


def test_matrix_schema_valid():
    eq("valid matrix has no errors", bl.validate_matrix(valid_matrix()), [])


def test_matrix_schema_teeth_false_pass():
    """TEETH: a false pass / unclassified error must not validate."""
    m = valid_matrix()
    m["runs"][0]["false_pass"] = True
    m["summary"]["false_pass"] = 1
    errs = bl.validate_matrix(m)
    check("false pass caught by validator",
          any("false_pass" in e for e in errs), repr(errs))

    m2 = valid_matrix()
    m2["runs"][0]["error"]["class"] = "unknown"
    m2["summary"]["unclassified"] = 1
    errs2 = bl.validate_matrix(m2)
    check("unclassified error caught by validator",
          any("unclassified" in e for e in errs2), repr(errs2))


def test_matrix_schema_missing_fields():
    m = valid_matrix()
    del m["runs"][0]["mem"]["cgroup_gib_min"]
    errs = bl.validate_matrix(m)
    check("missing cgroup_gib_min caught",
          any("cgroup_gib_min" in e for e in errs), repr(errs))

    m2 = valid_matrix()
    del m2["runs"][0]["negative"]
    errs2 = bl.validate_matrix(m2)
    check("missing negative column caught",
          any("negative" in e for e in errs2), repr(errs2))

    m3 = valid_matrix()
    m3["runs"][0]["state"] = "tepid"
    errs3 = bl.validate_matrix(m3)
    check("invalid state caught", any("state" in e for e in errs3), repr(errs3))


def test_write_json_roundtrip():
    with tempfile.TemporaryDirectory() as d:
        p = os.path.join(d, "sub", "matrix.json")
        bl.write_json(p, valid_matrix())
        check("write_json creates parent and file", os.path.exists(p))
        with open(p) as f:
            eq("write_json roundtrip", json.load(f)["schema"], bl.MATRIX_SCHEMA)
        check("no temp file left behind",
              not os.path.exists(p + ".tmp"))


def test_abba_is_real_time_based():
    # The default timing path records wall time when the callable returns None.
    r = bl.abba(lambda: time.sleep(0.001), lambda: time.sleep(0.001), pairs=3)
    check("wall-time samples are positive",
          all(row["value"] > 0 for row in r["rows"]))



# --------------------------------------------------------------------------- #
# W45: cgroup scope denominators (the M11 measurement)
# --------------------------------------------------------------------------- #

MIB = 1 << 20


def _fake_scope(cgroup_root, rel, *, anon=0.0, shmem=0.0, file=0.0, kernel=0.0,
                sock=0.0, pids=(), limit="8G"):
    """Create one fake cgroup v2 leaf with memory.current + memory.stat."""
    d = Path(cgroup_root) / rel.lstrip("/")
    d.mkdir(parents=True, exist_ok=True)
    total = anon + shmem + file + kernel + sock
    (d / "memory.current").write_text(str(int(total * MIB)))
    (d / "memory.stat").write_text(
        "anon %d\nfile %d\nshmem %d\nkernel %d\nkernel_stack 0\nsock %d\n"
        "slab 0\npagetables 0\n"
        % (int(anon * MIB), int((file + shmem) * MIB), int(shmem * MIB),
           int(kernel * MIB), int(sock * MIB)))
    if limit is not None:
        (d / "memory.max").write_text(limit)
    (d / "cgroup.procs").write_text(" ".join(str(p) for p in pids) + "\n")
    return d


def _fake_proc(proc_root, pid, ppid, comm, cgroup_rel, rss_mib=8.0):
    d = Path(proc_root) / str(pid)
    d.mkdir(parents=True, exist_ok=True)
    (d / "comm").write_text(comm + "\n")
    (d / "stat").write_text("%d (%s) S %d 0 0 0\n" % (pid, comm, ppid))
    (d / "cgroup").write_text("0::%s\n" % cgroup_rel)
    (d / "status").write_text("Name:\t%s\nVmRSS:\t%d kB\n" % (comm, int(rss_mib * 1024)))
    (d / "smaps_rollup").write_text("Rss: %d kB\nPss: %d kB\n"
                                    % (int(rss_mib * 1024), int(rss_mib * 1024)))
    return d


def _series(values, stat):
    """Two-sample constant series over 60 s."""
    return [{"t": 0.0, "bytes": values, "stat": dict(stat)},
            {"t": 60.0, "bytes": values, "stat": dict(stat)}]


def test_denominator_includes_guest_sibling():
    """TEETH: the guest lives in a SIBLING scope; dropping it halves the result."""
    job_stat = {"anon": 100 * MIB, "shmem": 10 * MIB, "file": 0, "kernel": 0}
    guest_stat = {"anon": 100 * MIB, "shmem": 10 * MIB, "file": 0, "kernel": 0}
    job = _series(110 * MIB, job_stat)
    guest = {"guest.scope": _series(110 * MIB, guest_stat)}
    d = bl.memory_denominators(job, guest, sampler_mib=0, window_s=60)

    job_only = 110 / 1024.0          # GiB.min for one 110 MiB holder for 1 min
    close("primary == job + guest anon+shmem", d["primary"]["gib_min"], 2 * job_only, 1e-9)
    close("scope_runner_formula is job only (the defect)",
          d["scope_runner_formula"]["gib_min"], job_only, 1e-9)
    check("guest cgroup counted in primary",
          d["primary"]["gib_min"] > d["scope_runner_formula"]["gib_min"] * 1.9,
          repr(d["primary"]))
    eq("guest scope listed", d["scope"]["guest_cgroups"], ["guest.scope"])
    eq("scope_count is job+guest", d["scope"]["scope_count"], 2)
    eq("primary is the gate value", d["primary"]["gate_uses"], True)
    for k in ("resident", "job_all_incl_file", "scope_runner_formula"):
        eq("%s is not a gate value" % k, d[k]["gate_uses"], False)
    close("resident adds kernel only", d["resident"]["gib_min"], 2 * job_only, 1e-9)
    close("job_all includes file (none here)", d["job_all_incl_file"]["gib_min"],
          2 * job_only, 1e-9)


def test_page_cache_not_charged_to_task():
    """TEETH: shared file cache must not move the primary denominator."""
    stat_small = {"anon": 50 * MIB, "shmem": 5 * MIB, "file": 100 * MIB, "kernel": 2 * MIB}
    stat_big = {"anon": 50 * MIB, "shmem": 5 * MIB, "file": 900 * MIB, "kernel": 2 * MIB}
    a = bl.memory_denominators(_series(157 * MIB, stat_small), {}, window_s=60)
    b = bl.memory_denominators(_series(957 * MIB, stat_big), {}, window_s=60)
    close("primary ignores page cache (a)", a["primary"]["gib_min"], 55 / 1024.0, 1e-9)
    close("primary ignores page cache (b)", b["primary"]["gib_min"], 55 / 1024.0, 1e-9)
    close("primary identical with 9x file cache", b["primary"]["gib_min"],
          a["primary"]["gib_min"], 1e-12)
    check("job_all does grow with file cache",
          b["job_all_incl_file"]["gib_min"] > a["job_all_incl_file"]["gib_min"] * 1.5)
    close("infra file cache = file - shmem (a)", a["infra_file_cache"]["gib_min"],
          95 / 1024.0, 1e-9)
    eq("infra file cache not charged", a["infra_file_cache"]["charged_to_task"], False)


def test_sampler_cost_excluded():
    """TEETH: subtract the sampler's ANON+SHMEM share, not its full RSS.

    The sampler's VmRSS also holds file-backed pages (the Python interpreter and
    benchlib.py), which are NOT part of an anon+shmem denominator. Subtracting
    VmRSS would over-subtract and inflate M11 (the W45 review defect).
    """
    stat = {"anon": 120 * MIB, "shmem": 0, "file": 0, "kernel": 0}
    d = bl.memory_denominators(_series(120 * MIB, stat), {}, sampler_mib=8,
                               sampler_rss_mib=24, window_s=60)
    close("raw primary is 120 MiB.min", d["primary"]["gib_min"], 120 / 1024.0, 1e-9)
    close("subtracted amount is anon+shmem (8 MiB)", d["sampler"]["overhead_gib_min"],
          8 / 1024.0, 1e-9)
    close("full-RSS sensitivity is reported, not subtracted",
          d["sampler"]["overhead_if_full_rss_gib_min"], 24 / 1024.0, 1e-9)
    close("primary excl sampler uses the same-type charge",
          d["primary"]["gib_min_excl_sampler"], (120 - 8) / 1024.0, 1e-9)
    eq("sampler anon+shmem recorded", d["sampler"]["anon_shmem_mib"], 8)
    eq("sampler rss recorded", d["sampler"]["rss_mib"], 24)
    eq("sampler marked excluded", d["sampler"]["excluded"], True)
    check("same-type exclusion is smaller than the RSS figure",
          d["sampler"]["overhead_gib_min"]
          < d["sampler"]["overhead_if_full_rss_gib_min"])
    check("exclusion is applied to the gate value",
          d["primary"]["gib_min_excl_sampler"] < d["primary"]["gib_min"])


def test_anon_shmem_probe_uses_rss_split():
    with tempfile.TemporaryDirectory() as d:
        proc = Path(d) / "proc"
        for pid, (anon, shmem, rss) in {700: (7.4, 0.3, 19.6),
                                        701: (5.0, 0.0, 5.0)}.items():
            _fake_proc(proc, pid, 1, "python3", "/job.scope", rss_mib=0)
            (proc / str(pid) / "status").write_text(
                "VmRSS:\t%d kB\nRssAnon:\t%d kB\nRssShmem:\t%d kB\n"
                % (int(rss * 1024), int(anon * 1024), int(shmem * 1024)))
        close("probe returns RssAnon+RssShmem, not VmRSS",
              bl._anon_shmem_mib(700, proc), 7.7, 2e-3)
        close("probe handles zero shmem", bl._anon_shmem_mib(701, proc), 5.0, 1e-3)
        close("full RSS still available", bl._rss_mib(700, proc), 19.6, 1e-3)


def test_scope_sampler_discovers_guest_sibling_and_ignores_flatpak():
    """TEETH: a fake cgroup tree with a guest sibling and a foreign bwrap."""
    with tempfile.TemporaryDirectory() as d:
        cg_root = Path(d) / "cgroup"
        proc_root = Path(d) / "proc"
        info = Path(d) / "info"
        info.mkdir()
        job_rel = "/job.slice/run-p100-i1.scope"
        guest_rel = "/guest.slice/run-p101-i2.scope"
        foreign_rel = "/flatpak/run-p900.scope"
        _fake_scope(cg_root, job_rel, anon=100, shmem=4, file=3, pids=[100, 101])
        _fake_scope(cg_root, guest_rel, anon=100, shmem=4, file=3, pids=[101])
        _fake_scope(cg_root, foreign_rel, anon=900, shmem=0, file=0, pids=[900])
        # pid 100 is the sampler/root; 101 is its child (bwrap) in the guest scope;
        # 900 is another user's bwrap whose ppid chain never reaches 100.
        _fake_proc(proc_root, 100, 1, "python3", job_rel)
        _fake_proc(proc_root, 101, 100, "bwrap", guest_rel)
        _fake_proc(proc_root, 900, 1, "bwrap", foreign_rel)

        sampler = bl.ScopeSampler(cg_root / job_rel.lstrip("/"), interval=0.02,
                                  info_dirs=[info], pss_every=1,
                                  proc_root=proc_root, cgroup_root=cg_root,
                                  root_pid=100)
        sampler.start()
        time.sleep(0.12)
        sampler.closing_sample()
        sampler.stop_event.set()
        sampler.join(timeout=5)

        guests = sampler.guest_cgroups()
        check("guest sibling scope discovered",
              any(g.endswith("guest.slice/run-p101-i2.scope") for g in guests), repr(guests))
        check("foreign flatpak bwrap NOT charged",
              not any("flatpak" in g for g in guests), repr(guests))
        check("sampler took >=2 job samples", len(sampler.samples) >= 2,
              "n=%d" % len(sampler.samples))
        check("closing sample appended", len(sampler.samples) >= 3)
        span = sampler.samples[-1]["t"] - sampler.samples[0]["t"]
        den = bl.memory_denominators(sampler.samples, sampler.guest,
                                     sampler_mib=1, window_s=span)
        job_only = 104 / 1024.0 * (span / 60.0)
        close("primary includes guest scope", den["primary"]["gib_min"],
              2 * job_only, 0.05 * job_only + 1e-12)
        check("primary > job-only formula", den["primary"]["gib_min"]
              > den["scope_runner_formula"]["gib_min"])


def test_concurrent_shared_data_evidence_is_owner_filtered():
    """TEETH: under N>1 one ACOS_ART_DATA is shared by every run.

    A sampler must only charge cgroup evidence written by ITS launcher (a
    descendant); another concurrent run's `<pid>-<seq>/cgroup` is not ours. If
    the filter is removed, each sampler charges the other run's guest and the
    denominator double counts.
    """
    with tempfile.TemporaryDirectory() as d:
        cg_root = Path(d) / "cgroup"
        proc_root = Path(d) / "proc"
        data = Path(d) / "data"
        mine_guest = "/guest.slice/run-p101-i1.scope"
        other_guest = "/guest.slice/run-p900-i1.scope"
        job_rel = "/job.slice/run-p100-i1.scope"
        _fake_scope(cg_root, job_rel, anon=10, pids=[100])
        _fake_scope(cg_root, mine_guest, anon=10, pids=[101])
        _fake_scope(cg_root, other_guest, anon=90, pids=[900])
        # One shared data tree: our launcher pid 101, another run's pid 900.
        (data / "runs" / "101-0").mkdir(parents=True)
        (data / "runs" / "101-0" / "cgroup").write_text("0::" + mine_guest + "\n")
        (data / "runs" / "900-0").mkdir(parents=True)
        (data / "runs" / "900-0" / "cgroup").write_text("0::" + other_guest + "\n")
        _fake_proc(proc_root, 100, 1, "python3", job_rel)
        _fake_proc(proc_root, 101, 100, "acos-art", mine_guest)
        _fake_proc(proc_root, 900, 1, "acos-art", other_guest)
        sampler = bl.ScopeSampler(cg_root / job_rel.lstrip("/"), interval=0.02,
                                  info_dirs=[data], pss_every=0,
                                  proc_root=proc_root, cgroup_root=cg_root,
                                  root_pid=100)
        sampler.start()
        time.sleep(0.06)
        sampler.closing_sample()
        sampler.stop_event.set()
        sampler.join(timeout=5)
        guests = sampler.guest_cgroups()
        check("shared-data: own guest evidence charged",
              any(g.endswith(mine_guest.lstrip("/")) for g in guests), repr(guests))
        check("shared-data: other concurrent run's guest NOT charged",
              not any("run-p900" in g for g in guests), repr(guests))


def test_host_scope_monitor_union_excludes_self_and_foreign():
    """TEETH: the host-level alternative discovers all jobs+guests independently."""
    with tempfile.TemporaryDirectory() as d:
        cg_root = Path(d) / "cgroup"
        proc_root = Path(d) / "proc"
        self_rel = "/suite.slice/run-p100.scope"
        job1 = "/job.slice/run-p101-i1.scope"
        guest1 = "/guest.slice/run-p102-i1.scope"
        job2 = "/job.slice/run-p103-i1.scope"
        guest2 = "/guest.slice/run-p104-i1.scope"
        foreign = "/flatpak/run-p900.scope"
        _fake_scope(cg_root, self_rel, anon=1, pids=[100])
        _fake_scope(cg_root, job1, anon=10, shmem=1, pids=[101])
        _fake_scope(cg_root, guest1, anon=20, shmem=2, pids=[102])
        _fake_scope(cg_root, job2, anon=30, shmem=3, pids=[103])
        _fake_scope(cg_root, guest2, anon=40, shmem=4, pids=[104])
        _fake_scope(cg_root, foreign, anon=900, pids=[900])
        _fake_proc(proc_root, 100, 1, "python3", self_rel)
        _fake_proc(proc_root, 101, 100, "python3", job1)
        _fake_proc(proc_root, 102, 101, "bwrap", guest1)
        _fake_proc(proc_root, 103, 100, "python3", job2)
        _fake_proc(proc_root, 104, 103, "bwrap", guest2)
        _fake_proc(proc_root, 900, 1, "bwrap", foreign)
        registry = Path(d) / "registry"
        registry.mkdir()
        job3 = "/job.slice/run-p105-i1.scope"
        _fake_scope(cg_root, job3, anon=50, shmem=5, pids=[])
        (registry / "T99-good-1.job").write_text(str(cg_root / job3.lstrip("/")))
        mon = bl.HostScopeMonitor(self_job=cg_root / self_rel.lstrip("/"),
                                  interval=0.02, discover_every=1,
                                  proc_root=proc_root, cgroup_root=cg_root,
                                  root_pid=100, registry_dirs=[registry])
        found = mon.discover()
        names = {str(c) for c in found}
        check("host monitor finds both job scopes",
              str(cg_root / job1.lstrip("/")) in names
              and str(cg_root / job2.lstrip("/")) in names, repr(names))
        check("host monitor finds both guest scopes",
              str(cg_root / guest1.lstrip("/")) in names
              and str(cg_root / guest2.lstrip("/")) in names, repr(names))
        check("host monitor excludes the suite's own cgroup",
              str(cg_root / self_rel.lstrip("/")) not in names, repr(names))
        check("host monitor excludes a foreign flatpak scope",
              str(cg_root / foreign.lstrip("/")) not in names, repr(names))
        check("host monitor picks up a registered job scope before its process appears",
              str(cg_root / job3.lstrip("/")) in names, repr(names))
        mon.sample_once()
        row = mon.samples[-1]
        close("host sample sums anon+shmem of all job+guest scopes",
              row["bytes"] / MIB, 10 + 1 + 20 + 2 + 30 + 3 + 40 + 4 + 50 + 5, 1e-6)
        eq("host sample lists 5 measured scopes", len(row["cgroups"]), 5)


def test_scope_agreement_noise_rule():
    a = bl.scope_agreement(1.000, 1.000, tolerance=0.05)
    check("identical integrals agree", a["within_noise"], repr(a))
    close("exact agreement rel diff is 0", a["rel_diff_pct"], 0.0)
    b = bl.scope_agreement(1.000, 1.030, tolerance=0.05)
    check("3 % difference is within 5 % noise", b["within_noise"], repr(b))
    c = bl.scope_agreement(1.000, 1.200, tolerance=0.05)
    check("20 % difference is NOT within noise", not c["within_noise"], repr(c))
    d = bl.scope_agreement(1.000, 1.050, sampler_overhead=0.020, tolerance=0.05)
    close("sampler charge is subtracted from the host figure",
          d["host_gib_min"], 1.030, 1e-9)
    check("host-minus-sampler within noise", d["within_noise"], repr(d))
    e = bl.scope_agreement(None, 1.0)
    check("missing per-run sum is not an agreement", not e["within_noise"], repr(e))


def test_concurrent_per_run_sum_equals_host_integral():
    """The two denominators are the same quantity for overlapping runs."""
    mib = MIB
    run_a = [{"t": 0.0, "bytes": 100 * mib, "stat": {"anon": 100 * mib, "shmem": 0}},
             {"t": 10.0, "bytes": 100 * mib, "stat": {"anon": 100 * mib, "shmem": 0}}]
    run_b = [{"t": 5.0, "bytes": 200 * mib, "stat": {"anon": 200 * mib, "shmem": 0}},
             {"t": 15.0, "bytes": 200 * mib, "stat": {"anon": 200 * mib, "shmem": 0}}]
    host = [{"t": 0.0, "bytes": 100 * mib, "stat": {"anon": 100 * mib, "shmem": 0}},
            {"t": 5.0, "bytes": 100 * mib, "stat": {"anon": 100 * mib, "shmem": 0}},
            {"t": 5.0, "bytes": 300 * mib, "stat": {"anon": 300 * mib, "shmem": 0}},
            {"t": 10.0, "bytes": 300 * mib, "stat": {"anon": 300 * mib, "shmem": 0}},
            {"t": 10.0, "bytes": 200 * mib, "stat": {"anon": 200 * mib, "shmem": 0}},
            {"t": 15.0, "bytes": 200 * mib, "stat": {"anon": 200 * mib, "shmem": 0}}]
    a = bl.stat_key_integral(run_a, bl.PRIMARY_KEYS)
    b = bl.stat_key_integral(run_b, bl.PRIMARY_KEYS)
    h = bl.stat_key_integral(host, bl.PRIMARY_KEYS)
    close("run A integral", a, 100 * 10 / 1024.0 / 60.0, 1e-12)
    close("run B integral", b, 200 * 10 / 1024.0 / 60.0, 1e-12)
    close("sum of per-run integrals", a + b, 3000 / 1024.0 / 60.0, 1e-12)
    close("host-level integral equals it", h, a + b, 1e-12)
    close("host vs per-run rel diff is zero",
          bl.scope_agreement(a + b, h)["rel_diff_pct"], 0.0, 1e-9)


def test_guest_evidence_file_route():
    with tempfile.TemporaryDirectory() as d:
        cg_root = Path(d) / "cgroup"
        _fake_scope(cg_root, "/guest.slice/x.scope", anon=10)
        ev = Path(d) / "run" / "cgroup"
        ev.parent.mkdir(parents=True)
        ev.write_text("0::/guest.slice/x.scope\n")
        cg = bl.cgroup_from_info_file(ev, cg_root)
        eq("evidence file resolves cgroup", str(cg), str(cg_root / "guest.slice/x.scope"))
        eq("missing evidence file -> None",
           bl.cgroup_from_info_file(Path(d) / "nope", cg_root), None)


def test_scope_reuse_is_rejected():
    """TEETH: two variants in one scope (the pre-W45 suite scope) must be refused."""
    rows = [{"variant": "T01-good-r1", "job_cgroup": "/a/one.scope", "sampler_pid": 11},
            {"variant": "T01-wrong-r1", "job_cgroup": "/a/one.scope", "sampler_pid": 12}]
    r = bl.check_variant_scopes(rows)
    check("reused scope rejected", not r["ok"], repr(r))
    check("reused scope error names the defect",
          any("reused" in e for e in r["errors"]), repr(r["errors"]))
    check("reused scope error names both variants",
          any("T01-good-r1" in e and "T01-wrong-r1" in e for e in r["errors"]))

    fresh = [{"variant": "T01-good-r1", "job_cgroup": "/a/one.scope", "sampler_pid": 11},
             {"variant": "T01-wrong-r1", "job_cgroup": "/a/two.scope", "sampler_pid": 12}]
    r2 = bl.check_variant_scopes(fresh)
    check("distinct fresh scopes accepted", r2["ok"], repr(r2["errors"]))
    eq("distinct scope count", r2["distinct_scopes"], 2)

    r3 = bl.check_variant_scopes([{"variant": "T01-good-r1", "job_cgroup": None}])
    check("missing scope rejected (no guarded scope)", not r3["ok"],
          repr(r3["errors"]))
    r4 = bl.check_variant_scopes([{"variant": "a", "job_cgroup": "/a", "sampler_pid": 7},
                                 {"variant": "b", "job_cgroup": "/b", "sampler_pid": 7}])
    check("reused sampler pid rejected", not r4["ok"], repr(r4["errors"]))


def test_prune_artifacts():
    with tempfile.TemporaryDirectory() as d:
        root = Path(d)
        (root / "verdict.json").write_text('{"verdict": "pass"}')
        (root / "art-frames").mkdir()
        (root / "art-frames" / "session-timing.json").write_text("{}")
        (root / "frame0.png").write_bytes(b"x" * MIB)
        (root / "frame1.png").write_bytes(b"x" * MIB)
        res = bl.prune_artifacts(root, keep=False, passed=True)
        check("pass keeps verdict.json", "verdict.json" in res["kept"], repr(res))
        check("pass keeps step timing",
              "art-frames/session-timing.json" in res["kept"], repr(res))
        check("pass drops frames", not (root / "frame0.png").exists()
              and not (root / "frame1.png").exists(), repr(res["kept"]))
        check("prune reports freed bytes", res["freed_bytes"] >= 2 * MIB, repr(res))
        check("prune is recorded", res["pruned"])

    with tempfile.TemporaryDirectory() as d:
        root = Path(d)
        (root / "verdict.json").write_text("{}")
        for i in range(3):
            (root / ("shot%d.png" % i)).write_bytes(b"x" * MIB)
        res = bl.prune_artifacts(root, keep=True, passed=True)
        eq("--keep-artifacts deletes nothing", res["deleted"], [])
        eq("all files kept", len([p for p in root.iterdir() if p.is_file()]), 4)
        check("keep mode not pruned", not res["pruned"])

    with tempfile.TemporaryDirectory() as d:
        root = Path(d)
        (root / "verdict.json").write_text("{}")
        for i in range(3):
            (root / ("fail%d.png" % i)).write_bytes(b"x" * MIB)
        res = bl.prune_artifacts(root, keep=False, passed=False, max_failure_mib=1.5)
        kept_png = [k for k in res["kept"] if k.endswith(".png")]
        deleted_png = [k for k in res["deleted"] if k.endswith(".png")]
        eq("fail run keeps a capped failure artifact", len(kept_png), 1)
        eq("fail run drops the rest", len(deleted_png), 2)
        check("fail run keeps the verdict", "verdict.json" in res["kept"])


def test_m11_ci_and_bar():
    costs = {"T%02d" % i: 0.0282 for i in range(1, 12)}
    verified = {k: True for k in costs}
    m = bl.m11_denominator(costs, verified)
    close("m11 = 11 / 0.3102", m["m11_tasks_per_gib_min"], 11 / 0.3102, 1e-6)
    check("m11 CI produced", m["ci95_low"] is not None and m["ci95_high"] is not None,
          repr(m))
    check("m11 CI brackets the point estimate",
          m["ci95_low"] <= m["m11_tasks_per_gib_min"] <= m["ci95_high"], repr(m))
    check("above-bar M11 passes", m["passes_bar"], repr(m))
    eq("required is bar*(1+noise), not the bare bar",
       round(m["required_tasks_per_gib_min"], 4), round(24.4 * 1.05, 4))

    expensive = {k: 0.05 for k in costs}
    m2 = bl.m11_denominator(expensive, verified)
    close("expensive suite m11", m2["m11_tasks_per_gib_min"], 11 / (11 * 0.05), 1e-9)
    check("M11 just under bar*(1+noise) fails the gate", not m2["passes_bar"], repr(m2))

    m3 = bl.m11_denominator({"T01": 0.01, "T02": None}, {"T01": True, "T02": True})
    eq("missing cost excluded, not estimated", m3["missing_tasks"], ["T02"])
    eq("missing cost task not verified in denominator", m3["tasks"], 1)
    close("m11 with one task", m3["m11_tasks_per_gib_min"], 1 / 0.01, 1e-9)


class _FakeClock:
    """Module-level stand-in for ``time`` so tick spacing is testable."""

    def __init__(self, now=1000.0, step=0.05):
        self.now = now
        self.step = step

    def time(self):
        self.now += self.step
        return self.now


def test_churn_sampler_sees_short_guests():
    """TEETH (S3c): a guest that lives 2 ticks must be in the integral.

    The legacy sampler discovered on a 1 s cadence; here the /proc walk is
    disabled (``slow_every`` huge) so only creation-time discovery can find the
    guest. A mutant that returns to the 1 s cadence drops the guest entirely and
    the integral falls back to the job-only value.
    """
    with tempfile.TemporaryDirectory() as d:
        slice_dir = Path(d) / "cg" / "app.slice"
        job = _fake_scope(slice_dir, "job.scope", anon=20)
        _fake_proc(Path(d) / "proc", 42, 1, "python3", "/app.slice/job.scope")
        s = bl.ChurnSampler(job, scope_parent=slice_dir,
                            proc_root=Path(d) / "proc", slow_every=10 ** 6,
                            root_pid=42, sampler_pid=42)
        saved = bl.time
        clock = bl.time = _FakeClock(step=bl.CHURN_INTERVAL_S)
        bl.time = clock
        try:
            s.sample_once()                   # tick 1: only the job scope
            eq("job only while no guest exists", s.samples[-1]["guest_scopes"], 0)
            _fake_scope(slice_dir, "run-7.scope", anon=50, pids=[70])
            _fake_proc(Path(d) / "proc", 70, 42, "dalvikvm64",
                       "/app.slice/run-7.scope", rss_mib=8.0)
            s.sample_once()
            eq("guest discovered on the tick after creation",
               s.samples[-1]["guest_scopes"], 1)
            eq("discovered through the slice, not the /proc walk",
               s.found_by.get("slice"), 1)
            close("guest charge is added", s.samples[-1]["guest_anon_shmem_mib"],
                  50.0, 1e-9)
            s.sample_once()
            eq("still counted on the next tick", s.samples[-1]["guest_scopes"], 1)
            with_guest = s.summary()["net_anon_shmem_gib_min"]
            (slice_dir / "run-7.scope" / "memory.stat").unlink()   # guest dies
            s.sample_once()
            eq("gone scope dropped (closing sample = the next job-only tick)",
               s.samples[-1]["guest_scopes"], 0)
            eq("scope recorded as dead",
               s.scopes[str(slice_dir / "run-7.scope")]["gone_t"] is not None, True)
            summ = s.summary()
        finally:
            bl.time = saved
        eq("guest counted in the integral", summ["guests_observed"], 1)
        eq("guest got 2 ticks", summ["samples_per_guest"]["median"], 2.0)
        # net = job(20) + guest(50) - sampler(8 MiB, anon only) over the window;
        # the guest's own integral is 50 MiB over ~0.05 s of tick spacing.
        expected = (50.0 / 1024.0) * (2 * bl.CHURN_INTERVAL_S) / 60.0
        close("guest integral is the sampled trapezoid", summ["guest_anon_shmem_gib_min"],
              expected, expected * 0.05)
        check("the guest moved the reported integral", with_guest > 0, repr(with_guest))
        check("guest part is a real share of the job+guest integral",
              summ["guest_anon_shmem_gib_min"] > 0.3 * summ["net_anon_shmem_gib_min"],
              repr(summ))


def test_churn_sampler_rejects_foreign_scopes():
    """A concurrent agent's guest scope in the same slice is never charged."""
    with tempfile.TemporaryDirectory() as d:
        slice_dir = Path(d) / "cg" / "app.slice"
        job = _fake_scope(slice_dir, "job.scope", anon=20)
        _fake_proc(Path(d) / "proc", 42, 1, "python3", "/app.slice/job.scope")
        _fake_scope(slice_dir, "run-9.scope", anon=999, pids=[90])
        _fake_proc(Path(d) / "proc", 90, 8, "dalvikvm64", "/app.slice/run-9.scope")
        s = bl.ChurnSampler(job, scope_parent=slice_dir,
                            proc_root=Path(d) / "proc", slow_every=10 ** 6,
                            root_pid=42, sampler_pid=42, max_pending=2)
        for _ in range(3):
            s.sample_once()
        eq("foreign scope never admitted", s.births, 0)
        eq("foreign scope stays out of every sample", s.samples[-1]["guest_scopes"], 0)
        check("foreign scope blacklisted after max_pending",
              str(slice_dir / "run-9.scope") in s.blacklisted, repr(s.blacklisted))
        check("no 999 MiB leak into the job reading",
              s.samples[-1]["net_anon_shmem_mib"] < 100, repr(s.samples[-1]))


def test_churn_sampler_pss_and_session_attribution():
    """Pss is charged per session id (run tree path), never per launcher child."""
    with tempfile.TemporaryDirectory() as d:
        slice_dir = Path(d) / "cg" / "app.slice"
        artifacts = str(Path(d) / "sessions")
        job = _fake_scope(slice_dir, "job.scope", anon=20)
        _fake_proc(Path(d) / "proc", 42, 1, "python3", "/app.slice/job.scope")
        _fake_scope(slice_dir, "run-3.scope", anon=60, pids=[31])
        p = _fake_proc(Path(d) / "proc", 31, 42, "dalvikvm64",
                       "/app.slice/run-3.scope", rss_mib=64.0)
        (p / "cmdline").write_bytes(
            ("bwrap --bind %s/agent-7/run1/app /data/app\0" % artifacts).encode())
        s = bl.ChurnSampler(job, scope_parent=slice_dir,
                            proc_root=Path(d) / "proc", slow_every=10 ** 6,
                            root_pid=42, sampler_pid=42, pss_every=1,
                            artifacts=[artifacts])
        s.sample_once()
        row = s.samples[-1]
        eq("session id read from the run tree path",
           s.scopes[str(slice_dir / "run-3.scope")]["session"], "agent-7")
        close("guest Pss sampled", row["guest_pss_mib"], 64.0, 1e-9)
        summ = s.summary(sessions=20)
        close("per-session Pss is not a zero reading",
              summ["per_session_pss_mib"], 64.0 / 20, 1e-9)
        eq("per-session attribution present",
           list(summ["session_anon_shmem_peak_mib"]), ["agent-7"])


def test_churn_sampler_coverage_counts_every_guest():
    """Coverage = observed guest scopes / launch+reset steps, and it is reported."""
    with tempfile.TemporaryDirectory() as d:
        slice_dir = Path(d) / "cg" / "app.slice"
        job = _fake_scope(slice_dir, "job.scope", anon=20)
        _fake_proc(Path(d) / "proc", 42, 1, "python3", "/app.slice/job.scope")
        s = bl.ChurnSampler(job, scope_parent=slice_dir,
                            proc_root=Path(d) / "proc", slow_every=10 ** 6,
                            root_pid=42, sampler_pid=42)
        for i, pid in enumerate([70, 71]):
            _fake_scope(slice_dir, "run-%d.scope" % i, anon=40, pids=[pid])
            _fake_proc(Path(d) / "proc", pid, 42, "dalvikvm64",
                       "/app.slice/run-%d.scope" % i)
        s.sample_once()
        summ = s.summary(expected_guests=2)
        eq("both short guests observed", summ["guests_observed"], 2)
        eq("full coverage when observed == expected", summ["coverage"], 1.0)
        eq("missing guests show up as under-coverage",
           s.summary(expected_guests=4)["coverage"], 0.5)
        eq("samples per guest reported", summ["samples_per_guest"]["min"], 1)


def test_churn_sampler_window_and_extras_compat():
    """The ported harnesses use WarmSampler's ``window``/``extras`` names."""
    with tempfile.TemporaryDirectory() as d:
        slice_dir = Path(d) / "cg" / "app.slice"
        job = _fake_scope(slice_dir, "job.scope", anon=20)
        _fake_proc(Path(d) / "proc", 42, 1, "python3", "/app.slice/job.scope")
        _fake_scope(slice_dir, "run-5.scope", anon=40, pids=[50])
        _fake_proc(Path(d) / "proc", 50, 42, "dalvikvm64",
                   "/app.slice/run-5.scope")
        s = bl.ChurnSampler(job, scope_parent=slice_dir,
                            proc_root=Path(d) / "proc", slow_every=10 ** 6,
                            root_pid=42, sampler_pid=42)
        s.sample_once()
        win = s.window(s.samples[0]["t"] - 1)
        check("window returns samples", win and win["samples"] == 1, repr(win))
        check("window carries net anon+shmem",
              win["net_anon_shmem_mib"]["n"] == 1, repr(win))
        eq("extras lists the live guest scope",
           s.extras, [str(slice_dir / "run-5.scope")])


def test_prune_screens_caps_per_task_pngs():
    """TEETH: an unbounded per-task PNG dir is capped to --keep-screens."""
    with tempfile.TemporaryDirectory() as d:
        root = Path(d)
        for i in range(30):
            (root / ("task-%03d.png" % i)).write_bytes(b"x" * 1024)
            os.utime(root / ("task-%03d.png" % i), (1000 + i, 1000 + i))
        res = bl.prune_screens(root, keep=20)
        eq("20 screenshots retained", len(list(root.rglob("*.png"))), 20)
        eq("10 screenshots deleted", len(res["deleted"]), 10)
        eq("newest sample retained", "task-029.png" in res["kept"], True)
        eq("oldest sample removed", "task-000.png" in res["deleted"], True)
        check("pruned bytes reported", res["freed_bytes"] == 10 * 1024)
        # keep=0 is the explicit "no screenshots" arm, not "keep everything".
        res0 = bl.prune_screens(root, keep=0)
        eq("keep=0 removes every png", len(list(root.rglob("*.png"))), 0)
        check("keep=0 deleted something", len(res0["deleted"]) == 20)


def test_sample_integral_forward_fill_guest():
    """A guest discovered late must be charged from its first sample only."""
    job = [{"t": 0, "bytes": 10, "stat": {"anon": 10}},
           {"t": 30, "bytes": 10, "stat": {"anon": 10}},
           {"t": 60, "bytes": 10, "stat": {"anon": 10}}]
    guest = {"g": [{"t": 30, "bytes": 10, "stat": {"anon": 10}},
                   {"t": 60, "bytes": 10, "stat": {"anon": 10}}]}
    rows = bl.combine_series(job, guest)
    eq("row count follows job samples", len(rows), 3)
    eq("guest absent before its first sample", rows[0]["stat"]["anon"], 10)
    eq("guest added after discovery", rows[1]["stat"]["anon"], 20)
    eq("guest still counted at the end", rows[2]["stat"]["anon"], 20)

def main():
    tests = [(name, fn) for name, fn in sorted(globals().items())
             if name.startswith("test_") and callable(fn)]
    print("benchlib tests against %s" % LIB_PATH)
    for name, fn in tests:
        fn()
    print("\n%d passed, %d failed" % (PASSES, FAILURES))
    return 1 if FAILURES else 0


if __name__ == "__main__":
    sys.exit(main())
