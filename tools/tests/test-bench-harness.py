#!/usr/bin/env python3
"""Fail-closed tests for tools/bench/acos_bench.py helpers.

Run: python3 tools/tests/test-bench-harness.py
"""
import importlib.util
import os
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.dirname(os.path.dirname(HERE))
sys.path.insert(0, os.path.join(REPO, "tools"))
sys.path.insert(0, os.path.join(REPO, "tools/art-runtime"))

import benchlib  # noqa: E402


def load(name, path):
    spec = importlib.util.spec_from_file_location(name, path)
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


ab = load("acos_bench", os.path.join(REPO, "tools/bench/acos_bench.py"))

PASSES = FAILURES = 0


def check(name, cond, detail=""):
    global PASSES, FAILURES
    if cond:
        PASSES += 1
        print("PASS %s" % name)
    else:
        FAILURES += 1
        print("FAIL %s%s" % (name, (" -- " + detail) if detail else ""))


def test_latency_never_records_failed_task():
    samples = {}
    ab.add(samples, "m6_reset_fast_ms", 0.009, False,
           {"before": "0", "after": "0"})
    ab.add(samples, "m6_reset_fast_ms", 195.0, True,
           {"before": "1", "after": "0"})
    ab.finish_samples(samples)
    row = samples["m6_reset_fast_ms"]
    check("failed latency sample is not in values", row["values"] == [195.0],
          repr(row))
    check("failed sample is counted as bad", row["bad"] == 1, repr(row))
    check("failed sample keeps its diagnostic", row["bad_values"][0]["value"] == 0.009,
          repr(row))
    check("stats use only the good sample", row["stats"]["n"] == 1, repr(row))


def test_non_latency_failure_still_visible():
    samples = {}
    ab.add(samples, "loop_n5_verified_rate", 0.8, False, {"verified": 40})
    row = samples["loop_n5_verified_rate"]
    check("a failed rate is still recorded", row["values"] == [0.8], repr(row))
    check("a failed rate is marked bad", row["bad"] == 1, repr(row))


def test_step_failures_carry_codes_and_count():
    f = ab.StepFailures()
    f.add("m6-fast-3", "reset", {"envelope": {"ok": False,
                                             "error": {"code": "SESSION_NOT_FOUND",
                                                       "message": "gone"}},
                                "wall_ms": 0.009})
    f.add("m6-fast-4", "launch", {"envelope": {"ok": True}, "wall_ms": 3.0},
          code="VERIFY_FAILED")
    s = f.summary()
    check("both failures counted", s["failed_steps"] == 2, repr(s))
    check("error codes grouped", s["failed_by_code"]["SESSION_NOT_FOUND"] == 1
          and s["failed_by_code"]["VERIFY_FAILED"] == 1, repr(s))
    check("rows keep task and step", s["rows"][0]["task"] == "m6-fast-3"
          and s["rows"][0]["step"] == "reset", repr(s))


def main():
    for name, fn in sorted(globals().items()):
        if name.startswith("test_") and callable(fn):
            fn()
    print("\n%d passed, %d failed" % (PASSES, FAILURES))
    return 1 if FAILURES else 0


if __name__ == "__main__":
    sys.exit(main())
