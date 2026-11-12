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
"""

import importlib.util
import json
import os
import sys
import tempfile
import time

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
