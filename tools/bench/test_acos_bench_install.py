#!/usr/bin/env python3
"""Unit tests for the APK-ready metric in tools/bench/acos_bench.py (MERGE040D(c)).

  python3 -m unittest discover -s tools/bench -p 'test_*.py'

M9 "APK ready" is the ACOS counterpart of the emulator's `install` row, and it is
read out of the launcher's own stage ladder (`ACOS_ART_STAGE_TIMING=1`):

    acos-art: stage launch-entry exec=12ms
    acos-art: stage apk-stage exec=88ms
    acos-art: stage native-libs exec=121ms
    acos-art: stage guest-exec exec=164ms

`launch-entry` -> `guest-exec` is "from the APK path to launchable"; the rungs in
between are ingest, staging and DEX/classpath prep. The tests below pin the
things that would otherwise rot silently: a missing rung must not produce a
number, a non-monotone ladder must not produce a number, and the substages must
be the deltas the report prints.
"""

import sys
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))

import acos_bench as B  # noqa: E402

LOG = """acos-art: stage launch-entry exec=12ms
acos-art: stage linkerconfig exec=40ms
acos-art: stage apk-stage exec=88ms
acos-art: stage native-libs exec=121ms
acos-art: stage guest-exec exec=164ms
acos-art: stage guest-spawned exec=190ms
"""


class ParseStages(unittest.TestCase):
    def test_reads_every_rung_in_order(self):
        stages = B.parse_stages(LOG)
        self.assertEqual([n for n, _ in stages],
                         ["launch-entry", "linkerconfig", "apk-stage",
                          "native-libs", "guest-exec", "guest-spawned"])
        self.assertEqual(stages[0], ("launch-entry", 12))

    def test_ignores_unrelated_lines(self):
        stages = B.parse_stages("noise\nacos-art: something else\n" + LOG)
        self.assertEqual(len(stages), 6)


class ApkReady(unittest.TestCase):
    def test_window_is_launch_entry_to_guest_exec(self):
        ready = B.apk_ready_from_stages(B.parse_stages(LOG))
        self.assertIsNotNone(ready)
        self.assertEqual(ready["ms"], 164 - 12)
        # Every rung inside the window is a substage delta, so the report can
        # say where the time went (the install row is a comparison, not a number).
        self.assertEqual(ready["substages"],
                         {"launch-entry": 0, "linkerconfig": 28,
                          "apk-stage": 48, "native-libs": 33, "guest-exec": 43})
        self.assertEqual(sum(ready["substages"].values()), ready["ms"])

    def test_no_guest_exec_is_no_number(self):
        stages = [("launch-entry", 12), ("apk-stage", 88)]
        self.assertIsNone(B.apk_ready_from_stages(stages),
                          "a launch that never reached the guest is not ready")

    def test_no_launch_entry_is_no_number(self):
        stages = [("apk-stage", 88), ("guest-exec", 164)]
        self.assertIsNone(B.apk_ready_from_stages(stages))

    def test_a_non_monotone_ladder_is_no_number(self):
        stages = [("launch-entry", 50), ("apk-stage", 90), ("guest-exec", 70)]
        self.assertIsNone(B.apk_ready_from_stages(stages),
                          "guest-exec before the stage it follows cannot be timed")

    def test_empty_ladder_is_no_number(self):
        self.assertIsNone(B.apk_ready_from_stages([]))


if __name__ == "__main__":
    unittest.main()
