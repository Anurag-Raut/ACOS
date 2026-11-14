#!/usr/bin/env python3
"""Unit tests for tools/bench/bench_lattice.py (PERF-REL3 lattice math).

  python3 -m unittest discover -s tools/bench -p 'test_*.py'
"""

import math
import random
import sys
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))

import bench_lattice as L  # noqa: E402

FRAME = 1000.0 / 60.0


def lattice(origin, slots, n_per_slot, period=FRAME, jitter=0.0, seed=1):
    """Synthetic fast-settle data: n_per_slot samples in each listed slot."""
    rng = random.Random(seed)
    out = []
    for slot in slots:
        for _ in range(n_per_slot):
            out.append(origin + slot * period + rng.uniform(-jitter, jitter))
    return out


class TrimmedMeanTest(unittest.TestCase):
    def test_hand_computed(self):
        # n=20, 5 % -> 1 per side -> mean(2..19) = 10.5
        vals = list(range(1, 21))
        self.assertAlmostEqual(L.trimmed_mean(vals, 0.05), 10.5)
        self.assertEqual(L.trimmed_count(vals, 0.05), 1)

    def test_zero_fraction_is_mean(self):
        vals = [3.0, 1.0, 2.0, 10.0]
        self.assertAlmostEqual(L.trimmed_mean(vals, 0.0), 4.0)

    def test_removes_outliers(self):
        # One high outlier, one high-side near outlier: 5 % of 20 trims the top
        # and bottom sample, so the 1000 must be gone.
        vals = [1000.0, 1.11, 1.1, 0.9, 1.05, 1.02, 0.98, 1.01, 0.99, 1.03,
                1.12, 1.04, 1.06, 0.97, 1.07, 1.08, 1.09, 1.0, 1.09, 1.12]
        mean = sum(vals) / len(vals)
        trimmed = L.trimmed_mean(vals, 0.05)
        self.assertLess(trimmed, mean)          # the 1000 is trimmed away
        self.assertLess(trimmed, 2.0)

    def test_degenerate_inputs(self):
        self.assertIsNone(L.trimmed_mean([]))
        self.assertIsNone(L.trimmed_mean(None))
        self.assertEqual(L.trimmed_mean([5.0]), 5.0)
        self.assertEqual(L.trimmed_mean([None, "x", 2.0]), 2.0)
        # fraction large enough to remove everything is clamped, never empty
        self.assertEqual(L.trimmed_mean([1.0, 2.0, 3.0], 0.9), 2.0)


class FramePeriodTest(unittest.TestCase):
    def test_60hz_three_modes(self):
        vals = lattice(155.0, [0, 1, 2], 30, jitter=0.5, seed=3)
        period = L.estimate_frame_period(vals)
        self.assertIsNotNone(period)
        self.assertLess(abs(period - FRAME), 0.3)

    def test_120hz(self):
        vals = lattice(120.0, [0, 1, 2, 3], 20, period=1000.0 / 120.0,
                       jitter=0.3, seed=4)
        period = L.estimate_frame_period(vals)
        self.assertIsNotNone(period)
        self.assertLess(abs(period - 1000.0 / 120.0), 0.3)

    def test_continuum_has_no_period(self):
        rng = random.Random(11)
        vals = [100.0 + rng.uniform(0.0, 120.0) for _ in range(200)]
        self.assertIsNone(L.estimate_frame_period(vals))

    def test_one_slot_is_not_a_lattice(self):
        vals = lattice(155.0, [0], 40, jitter=0.4, seed=5)
        self.assertIsNone(L.estimate_frame_period(vals))

    def test_too_few_values(self):
        self.assertIsNone(L.estimate_frame_period([]))
        self.assertIsNone(L.estimate_frame_period([155.0, 171.0]))


class SlotTest(unittest.TestCase):
    def test_rounding(self):
        self.assertEqual(L._slot_index(155.0, 155.0, FRAME), 0)
        self.assertEqual(L._slot_index(155.0 + FRAME, 155.0, FRAME), 1)
        self.assertEqual(L._slot_index(155.0 + 2 * FRAME, 155.0, FRAME), 2)
        self.assertEqual(L._slot_index(155.0 + 5.0, 155.0, FRAME), 0)

    def test_shares_equal_modes(self):
        vals = lattice(44.0, [0, 1, 2], 20, jitter=0.3, seed=7)
        shares = L.slot_shares(vals)
        self.assertEqual(sorted(shares), [0, 1, 2])
        for share in shares.values():
            self.assertAlmostEqual(share, 1.0 / 3.0, places=6)

    def test_shared_origin_makes_arms_comparable(self):
        # Same lattice, different mode occupancy: arm A mostly slot 0, arm B
        # mostly slot 1. Scored on one origin/period the shares must line up.
        a = lattice(155.0, [0], 30, jitter=0.2, seed=8) + \
            lattice(155.0, [1], 10, jitter=0.2, seed=9)
        b = lattice(155.0, [0], 10, jitter=0.2, seed=10) + \
            lattice(155.0, [1], 30, jitter=0.2, seed=11)
        origin, period = 155.0, FRAME
        sa = L.slot_shares(a, period=period, origin=origin)
        sb = L.slot_shares(b, period=period, origin=origin)
        self.assertAlmostEqual(sa[0], 0.75)
        self.assertAlmostEqual(sb[0], 0.25)

    def test_detail_first_slot_share(self):
        vals = lattice(155.0, [0], 15, jitter=0.2, seed=12) + \
            lattice(155.0, [2], 5, jitter=0.2, seed=13)
        detail = L.slot_detail(vals, period=FRAME, origin=155.0)
        self.assertAlmostEqual(detail["first_slot_share"], 0.75)
        self.assertEqual([row["slot"] for row in detail["slots"]], [0, 2])
        self.assertEqual(sum(row["n"] for row in detail["slots"]), 20)


class AnalyseTest(unittest.TestCase):
    def test_record_shape_and_values(self):
        vals = lattice(155.0, [0, 1, 2], 20, jitter=0.4, seed=14)
        rec = L.analyse(vals)
        for key in ("n", "samples", "frame_period_ms", "frame_period_source",
                    "origin_ms", "trimmed_mean", "median", "slots",
                    "slot_shares", "first_slot_share"):
            self.assertIn(key, rec)
        self.assertEqual(rec["n"], 60)
        self.assertEqual(rec["frame_period_source"], "measured")
        self.assertLess(abs(rec["trimmed_mean"] - (155.0 + FRAME)), 1.0)
        self.assertEqual(sorted(rec["slot_shares"]), ["0", "1", "2"])

    def test_vsync_period_wins(self):
        vals = lattice(155.0, [0, 1], 20, jitter=0.3, seed=15)
        rec = L.analyse(vals, vsync_period_ms=16.0)
        self.assertEqual(rec["frame_period_source"], "compositor")
        self.assertEqual(rec["frame_period_ms"], 16.0)

    def test_fallback_on_flat_data(self):
        rec = L.analyse([155.1, 155.2, 155.05, 155.15])
        self.assertEqual(rec["frame_period_source"], "fallback")
        self.assertAlmostEqual(rec["frame_period_ms"],
                               round(L.DEFAULT_FRAME_PERIOD_MS, 3))
        self.assertEqual(rec["first_slot_share"], 1.0)

    def test_empty(self):
        rec = L.analyse([])
        self.assertEqual(rec["n"], 0)
        self.assertIsNone(rec["trimmed_mean"])
        self.assertEqual(rec["slots"], [])


if __name__ == "__main__":
    unittest.main()
