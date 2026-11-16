#!/usr/bin/env python3
"""Unit tests for tools/bench/ab-compare.py (PERF-REL3 A/B gate).

  python3 -m unittest discover -s tools/bench -p 'test_*.py'
"""

import importlib.util
import json
import random
import sys
import tempfile
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))

# The tool is named ab-compare.py (a CLI name), so load it by path.
_spec = importlib.util.spec_from_file_location("ab_compare",
                                               HERE / "ab-compare.py")
ab_compare = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(ab_compare)

FRAME = 1000.0 / 60.0


def samples(origin, slots, per_slot, jitter=0.3, seed=1):
    rng = random.Random(seed)
    out = []
    for slot in slots:
        for _ in range(per_slot):
            out.append(round(origin + slot * FRAME + rng.uniform(-jitter, jitter), 3))
    return out


def doc(metrics, commit="0" * 40):
    return {"schema": "acos-bench-baseline/2", "commit": commit,
            "utc": "2026-11-15T00:00:00Z", "metrics": metrics}


def metric(values, rep_p50=None, category="latency", threshold=None):
    m = {"category": category, "unit": "ms", "p50": sorted(values)[len(values) // 2],
         "n": len(values), "samples": values,
         "rep_p50": rep_p50 if rep_p50 is not None else [sorted(values)[len(values) // 2]]}
    if threshold:
        m.update(threshold)
    return m


class OldGateTest(unittest.TestCase):
    def test_within_noise(self):
        a = {"rep_p50": [155.0, 171.0, 187.0], "category": "latency"}
        b = {"rep_p50": [157.0, 172.0, 189.0], "category": "latency"}
        row = ab_compare.old_gate(a, b, "m4_launch_fast_ms")
        self.assertEqual(row["verdict"], "within-noise")

    def test_regression_uses_doc_threshold(self):
        a = {"rep_p50": [100.0, 100.0], "category": "latency"}
        b = {"rep_p50": [120.0, 120.0], "category": "latency",
             "threshold_pct": 5.0, "threshold_abs": 2.0}
        row = ab_compare.old_gate(a, b, "m4_launch_strict_ms")
        self.assertEqual(row["verdict"], "REGRESSION")
        self.assertAlmostEqual(row["delta_pct"], 20.0)

    def test_missing(self):
        row = ab_compare.old_gate({"rep_p50": []}, {"rep_p50": [1.0]}, "x")
        self.assertEqual(row["verdict"], "missing")


class LatticeGateTest(unittest.TestCase):
    def test_identical_is_within_noise(self):
        a = {"samples": samples(155.0, [0, 1, 2], 20, seed=2)}
        b = {"samples": samples(155.0, [0, 1, 2], 20, seed=3)}
        row = ab_compare.lattice_gate(a, b, "m4_launch_fast_ms")
        self.assertEqual(row["verdict"], "within-noise")
        self.assertFalse(row["slot_shift"])
        self.assertAlmostEqual(row["first_slot_share_a"], row["first_slot_share_b"])
        self.assertLess(abs(row["frame_period_ms"] - FRAME), 0.4)

    def test_one_frame_shift_is_a_regression(self):
        # One whole frame slower: the old p50 gate can call this noise when the
        # modes split, but the pooled trimmed mean and the slot shift both see it.
        a = {"samples": samples(155.0, [0, 1, 2], 20, seed=4)}
        b = {"samples": samples(155.0 + FRAME, [0, 1, 2], 20, seed=5)}
        row = ab_compare.lattice_gate(a, b, "m4_launch_fast_ms")
        self.assertEqual(row["verdict"], "REGRESSION")
        self.assertGreater(row["delta"], 2.0)
        self.assertGreater(row["delta_pct"], 5.0)
        self.assertLessEqual(abs(row["first_slot_delta_pp"]), 100.0)
        self.assertGreaterEqual(abs(row["first_slot_delta_pp"]), 10.0)
        self.assertTrue(row["slot_shift"])

    def test_small_shift_below_both_floors(self):
        # +1 ms and +0.6 %: inside the +5 % / +2 ms band.
        a = {"samples": samples(155.0, [0, 1], 20, seed=6)}
        b = {"samples": samples(156.0, [0, 1], 20, seed=7)}
        row = ab_compare.lattice_gate(a, b, "m4_launch_fast_ms")
        self.assertEqual(row["verdict"], "within-noise")

    def test_pct_above_but_abs_below(self):
        # tiny metric (tap): +3 ms on 45 ms is +6.7 % but only just over 2 ms;
        # make it +2.5 ms -> +5.6 %: both floors must be exceeded, so this is a
        # regression, while +1.9 ms is not.
        a = {"samples": samples(44.0, [0, 1], 20, seed=8)}
        b = {"samples": samples(45.9, [0, 1], 20, seed=9)}
        self.assertEqual(ab_compare.lattice_gate(
            a, b, "m8_tap_counter_fast_ms")["verdict"], "within-noise")
        c = {"samples": samples(46.6, [0, 1], 20, seed=10)}
        self.assertEqual(ab_compare.lattice_gate(
            a, c, "m8_tap_counter_fast_ms")["verdict"], "REGRESSION")

    def test_slot_shift_flagged_without_trimmed_mean_move(self):
        # Same trimmed mean (modes are symmetric) but a different first-slot
        # occupancy: the share flag fires, the trimmed-mean verdict does not.
        a = {"samples": samples(155.0, [0], 20, seed=11) +
                      samples(155.0, [1], 10, seed=12) +
                      samples(155.0, [2], 20, seed=13)}
        b = {"samples": samples(155.0, [0], 10, seed=14) +
                      samples(155.0, [1], 30, seed=15) +
                      samples(155.0, [2], 10, seed=16)}
        row = ab_compare.lattice_gate(a, b, "m4_launch_fast_ms")
        self.assertEqual(row["verdict"], "within-noise")
        self.assertTrue(row["slot_shift"])
        self.assertLessEqual(row["first_slot_delta_pp"], -10.0)

    def test_shared_lattice_across_arms(self):
        # Arm B sits one whole slot up: scored on the union lattice, no
        # per-arm origin can hide it.
        a = {"samples": samples(155.0, [0, 1], 20, seed=15)}
        b = {"samples": samples(155.0, [1, 2], 20, seed=16)}
        row = ab_compare.lattice_gate(a, b, "m4_launch_fast_ms")
        self.assertEqual(row["first_slot_share_a"], 0.5)
        self.assertEqual(row["first_slot_share_b"], 0.0)
        self.assertLess(row["slot_delta_pp"]["0"], -40.0)

    def test_old_baseline_without_samples(self):
        row = ab_compare.lattice_gate({"samples": []}, {"samples": []}, "x")
        self.assertEqual(row["verdict"], "no-samples")
        self.assertIsNone(row["trim_a"])


class CliTest(unittest.TestCase):
    def _write(self, tmp, name, payload):
        path = Path(tmp) / name
        path.write_text(json.dumps(payload))
        return str(path)

    def test_old_schema_docs_still_parse(self):
        old = {"schema": "acos-bench-baseline/2", "commit": "a" * 40,
               "metrics": {"m4_launch_fast_ms": {
                   "category": "latency", "unit": "ms", "p50": 170.0, "n": 20,
                   "rep_p50": [155.0, 171.0, 187.0]}}}
        with tempfile.TemporaryDirectory() as tmp:
            a = self._write(tmp, "a.json", old)
            b = self._write(tmp, "b.json", old)
            rc = ab_compare.main(["--a", a, "--b", b, "--metrics",
                                  "m4_launch_fast_ms", "--quiet"])
        self.assertEqual(rc, 0)

    def test_regression_exit_code_and_json(self):
        a = doc({"m4_launch_fast_ms": metric(samples(155.0, [0, 1, 2], 20, seed=20))})
        b = doc({"m4_launch_fast_ms": metric(
            samples(155.0 + FRAME, [0, 1, 2], 20, seed=21))}, commit="b" * 40)
        with tempfile.TemporaryDirectory() as tmp:
            pa = self._write(tmp, "a.json", a)
            pb = self._write(tmp, "b.json", b)
            out = str(Path(tmp) / "ab.json")
            rc = ab_compare.main(["--a", pa, "--b", pb, "--json", out,
                                  "--quiet"])
            payload = json.loads(Path(out).read_text())
        self.assertEqual(rc, 1)
        self.assertEqual(payload["rows"][0]["lattice"]["verdict"], "REGRESSION")
        self.assertEqual(payload["gate"]["status"], "proposal (PERF-REL3)")


if __name__ == "__main__":
    unittest.main()
