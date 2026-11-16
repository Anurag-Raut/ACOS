#!/usr/bin/env python3
"""Unit tests for tools/bench/bench_reply.py (BENCHFIX).

  python3 -m unittest discover -s tools/bench -p 'test_*.py'

The fixtures below are the two reply shapes the bench must read, reduced to the
fields it uses:

* v0.2.2 (and older): the full tree is inline, there is no `tree_path`;
* rel-030 (REL3C): `result.tree` is a compact page and the full tree is a file
  named by `result.tree_path`.

Each test pins one way the bench used to rot silently: a compact page mistaken
for the screen, a missing `tree_path`, an empty frame md5 (the
`two consecutive settled captures differ: [None, None]` failure), a caller path
outside the session's artifacts (FB2c), a renamed settle field, and P7's
hard-coded node count (the `count='0' nodes=14` refusal).
"""

import json
import sys
import tempfile
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))

import bench_reply as R  # noqa: E402


def counter_nodes(count_text="0"):
    """The counter fixture's tree as ART serializes it (14 nodes, ids matter)."""
    def node(rid, cls, text=None, children=None, bounds="[0,0][10,10]"):
        out = {"id": rid.replace(":", "_"), "class": cls, "bounds": bounds,
               "package": R.COUNTER_PACKAGE, "visible": True,
               "children": children or []}
        if rid:
            out["resource_id"] = "dev.acos.fixtures.counter:id/" + rid
        if text is not None:
            out["text"] = text
        return out

    return {"max_nodes": 2000, "truncated": False, "nodes": [
        node("", "android.widget.FrameLayout", children=[
            node("", "android.widget.LinearLayout", children=[
                node("count", "android.widget.TextView", count_text),
                node("inc", "android.widget.Button", "increment"),
                node("reset", "android.widget.Button", "reset"),
            ]),
        ]),
    ]}


def envelope(result):
    return {"ok": True, "result": result, "metrics": {"wall_ms": 1.0}}


def launch_inline(count_text="0", pixels=True, **extra):
    result = {
        "session_id": "s_ref", "launched": True,
        "frame": {"frame_seq": 5, "md5": "a" * 32 if pixels else "",
                  "pixels": pixels},
        "tree": counter_nodes(count_text),
        "settle": {"mode": "strict", "settled": True, "bound": "change-draw"},
    }
    result.update(extra)
    return envelope(result)


def compact_page(node_count=3):
    return {"compact": True, "cursor": 0, "next_cursor": None,
            "node_count": node_count, "returned": node_count,
            "nodes": [{"id": "count_1", "resource_id": "dev.acos.fixtures.counter:id/count",
                       "class": "android.widget.TextView", "text": "0",
                       "bounds": "[0,0][10,10]"}]}


def launch_compact(count_text="0", tree_path=None, pixels=True):
    result = {
        "session_id": "s_ref", "launched": True,
        "frame": {"frame_seq": 5, "md5": "a" * 32 if pixels else "",
                  "pixels": pixels},
        "tree": compact_page(),
        "tree_node_count": 3,
        "tree_path": tree_path,
        "settle": {"mode": "strict", "settled": True, "bound": "change-draw"},
    }
    return envelope(result)


class TreeShapeTest(unittest.TestCase):
    def test_inline_full_tree_is_used_as_is(self):
        res = R.result_of(launch_inline(), "acos_launch")
        tree = R.full_tree(res, "acos_launch")
        self.assertEqual(len(R.nodes(tree)), 5)
        self.assertEqual(R.count_text(tree), "0")
        R.require_counter_screen(tree, "reference")

    def test_compact_page_resolves_to_the_full_tree_file(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "tree" / "5.json"
            path.parent.mkdir()
            path.write_text(json.dumps(counter_nodes("0")))
            envelope_ = launch_compact(tree_path=str(path))
            res, tree = R.require_launch(envelope_, "acos_launch")
            # NOT the compact page: the full fixture tree (5 recursive nodes here).
            self.assertNotIn("compact", tree)
            self.assertEqual(len(R.nodes(tree)), 5)
            R.require_counter_screen(tree, "reference")
            self.assertEqual(res["tree"]["node_count"], 3)

    def test_compact_page_without_tree_path_fails_loudly(self):
        # This is the integration failure: a page walked as if it were the screen.
        with self.assertRaises(R.ReplyShapeError) as ctx:
            R.full_tree(R.result_of(launch_compact(tree_path=None), "acos_launch"),
                        "acos_launch")
        self.assertIn("names no `tree_path`", str(ctx.exception))

    def test_tree_path_naming_a_compact_page_is_refused(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "5.json"
            path.write_text(json.dumps(compact_page()))
            with self.assertRaises(R.ReplyShapeError) as ctx:
                R.full_tree(R.result_of(launch_compact(tree_path=str(path)),
                                        "acos_launch"), "acos_launch")
        self.assertIn("is not a full tree", str(ctx.exception))

    def test_missing_tree_path_file_fails_loudly(self):
        with self.assertRaises(R.ReplyShapeError):
            R.full_tree(R.result_of(launch_compact(tree_path="/nope/5.json"),
                                    "acos_launch"), "acos_launch")

    def test_tree_without_nodes_fails_loudly(self):
        result = launch_inline()["result"]
        result["tree"] = {"nothing": True}
        with self.assertRaises(R.ReplyShapeError) as ctx:
            R.full_tree(result, "acos_launch")
        self.assertIn("neither a full tree", str(ctx.exception))


class CounterScreenTest(unittest.TestCase):
    def test_count_zero_screen_passes(self):
        R.require_counter_screen(counter_nodes("0"), "reference")

    def test_wrong_count_is_refused(self):
        with self.assertRaises(R.ReplyShapeError) as ctx:
            R.require_counter_screen(counter_nodes("7"), "reference")
        self.assertIn("count text is '7'", str(ctx.exception))

    def test_missing_fixture_id_is_refused(self):
        tree = counter_nodes("0")
        R.nodes(tree)[2]["resource_id"] = "dev.acos.fixtures.counter:id/other"
        with self.assertRaises(R.ReplyShapeError):
            R.require_counter_screen(tree, "reference")

    def test_skeleton_is_refused(self):
        tree = {"truncated": False, "nodes": [{"class": "android.widget.FrameLayout"}]}
        with self.assertRaises(R.ReplyShapeError) as ctx:
            R.require_counter_screen(tree, "reference")
        self.assertIn("skeleton", str(ctx.exception))

    def test_truncated_tree_is_refused(self):
        tree = counter_nodes("0")
        tree["truncated"] = True
        with self.assertRaises(R.ReplyShapeError):
            R.require_counter_screen(tree, "reference")

    def test_more_nodes_than_p6_measured_still_passes(self):
        # The old P7 rule was `len(nodes) == 12`; the same screen is 14 nodes
        # since v0.2.2. A measured node count is not a correctness rule.
        tree = counter_nodes("0")
        tree["nodes"][0]["children"].append({"class": "android.widget.TextView",
                                             "text": "extra", "bounds": "[0,0][1,1]"})
        R.require_counter_screen(tree, "reference")


class FrameTest(unittest.TestCase):
    def test_pixel_frame_md5_is_read(self):
        res = R.result_of(launch_inline(pixels=True), "acos_launch")
        self.assertEqual(R.frame_md5(res, "acos_launch"), "a" * 32)

    def test_metadata_frame_without_pixels_is_an_error(self):
        # The `two consecutive settled captures differ: [None, None]` failure:
        # a launch that did not ask for pixels has an empty md5, so a comparison
        # of two such replies is meaningless. Fail at the field, not with None.
        with self.assertRaises(R.ReplyShapeError) as ctx:
            R.frame_md5(R.result_of(launch_inline(pixels=False), "acos_launch"),
                        "acos_launch")
        self.assertIn("pixels:true", str(ctx.exception))

    def test_identical_captures(self):
        self.assertEqual(R.identical_captures(["a", "a"], "reference"), "a")

    def test_none_captures_are_refused(self):
        with self.assertRaises(R.ReplyShapeError) as ctx:
            R.identical_captures([None, None], "reference")
        self.assertIn("[None, None]", str(ctx.exception))

    def test_differing_captures_are_refused(self):
        with self.assertRaises(R.ReplyShapeError):
            R.identical_captures(["a", "b"], "reference")

    def test_capture_reply_needs_frame_seq(self):
        result = {"frame": {"md5": "a" * 32}}
        with self.assertRaises(R.ReplyShapeError):
            R.require_capture(envelope(result), "acos_observe")


class ScreenshotTest(unittest.TestCase):
    def test_reported_path_is_used(self):
        path, meta = R.screenshot_path(
            {"screenshot": {"path": "/tmp/x/m5.png", "bytes": 10}}, "acos_observe")
        self.assertEqual(str(path), "/tmp/x/m5.png")
        self.assertEqual(meta["bytes"], 10)

    def test_absent_screenshot_block_is_an_error(self):
        with self.assertRaises(R.ReplyShapeError):
            R.screenshot_path({}, "acos_observe")


class VerbReplyTest(unittest.TestCase):
    def test_launch_needs_launched_settle_frame(self):
        result = launch_inline()["result"]
        del result["settle"]
        with self.assertRaises(R.ReplyShapeError) as ctx:
            R.require_launch(envelope(result), "acos_launch")
        self.assertIn("settle", str(ctx.exception))

    def test_renamed_launched_field_fails_loudly(self):
        result = launch_inline()["result"]
        result["started"] = result.pop("launched")
        with self.assertRaises(R.ReplyShapeError) as ctx:
            R.require_launch(envelope(result), "acos_launch")
        self.assertIn("launched", str(ctx.exception))

    def test_reset_needs_state_and_frame_seq(self):
        result = {"state_reset": True, "data_cleared": True,
                  "tree_path": None, "tree": counter_nodes("0"),
                  "settle": {"mode": "strict", "settled": True, "bound": "x"}}
        with self.assertRaises(R.ReplyShapeError):
            R.require_reset(envelope(result), "acos_reset")

    def test_tap_needs_the_phase_fields(self):
        with self.assertRaises(R.ReplyShapeError):
            R.require_tap(envelope({"settle": {"mode": "strict"}}), "acos_tap")
        # The verdict is the TOP-LEVEL `state_changed`. settle.state_changed is
        # null in strict mode (acos-art SettleTiming), so a bench that reads it
        # from settle checks nothing: `None is not False` was always true.
        with self.assertRaises(R.ReplyShapeError) as ctx:
            R.require_tap(envelope({"settle": {"mode": "strict", "settled": True,
                                               "bound": "state-commit",
                                               "state_changed": None,
                                               "ack_ms": 1.0,
                                               "frame_ms": 2.0}}), "acos_tap")
        self.assertIn("result.state_changed", str(ctx.exception))
        # A present-but-false verdict is a legitimate failed tap, not a shape
        # error: the field exists, the check still fails the step.
        res, settle = R.require_tap(
            envelope({"state_changed": False,
                      "settle": {"mode": "strict", "settled": True,
                                 "bound": "state-commit", "ack_ms": 1.0,
                                 "frame_ms": 2.0, "state_changed": None}}),
            "acos_tap")
        self.assertFalse(res["state_changed"])
        self.assertIsNone(settle["state_changed"])
        self.assertEqual(settle["ack_ms"], 1.0)

    def test_failed_reply_is_a_shape_error_not_a_none(self):
        with self.assertRaises(R.ReplyShapeError) as ctx:
            R.result_of({"ok": False, "error": {"code": "INVALID_ARGUMENT",
                                               "message": "screenshot outside"}},
                        "acos_observe")
        self.assertIn("INVALID_ARGUMENT", str(ctx.exception))

    def test_destroy_needs_teardown_verified(self):
        with self.assertRaises(R.ReplyShapeError):
            R.require_destroy(envelope({"teardown": {"lifecycle": "x"}}),
                              "acos_destroy")
        _, teardown = R.require_destroy(
            envelope({"teardown": {"verified": True, "lifecycle": "x"}}),
            "acos_destroy")
        self.assertTrue(teardown["verified"])


if __name__ == "__main__":
    unittest.main()
