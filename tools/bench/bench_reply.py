#!/usr/bin/env python3
"""bench_reply.py -- the bench's contract with tree-bearing product replies.

Why this module exists (BENCHFIX): the bench used to read replies by a guessed
shape and score `None` when the shape moved.  Two replies changed since the v0.2.1
A/B and killed `phase_latency` / `phase_cold` without a single gate line saying
so:

* REL3C (rel-030): tree-bearing replies are compact by default.  `result.tree` is
  a page of visible text nodes (`compact:true`, `cursor`, `next_cursor`) and the
  **full tree is written per session** and named by `result.tree_path`.  Both the
  CLI daemon and MCP use it (`crates/acos-protocol/src/reply.rs`).  A bench that
  walks `result.tree` then sees a page, not the screen.
* FB2c (v0.2.2): `screenshot` is either `true` (write under the session's
  artifacts, report `result.screenshot.path`) or a path *inside* that artifacts
  dir.  The bench's old `screenshot: scratch/ref-1.png` is now an argument error,
  so the capture reply had no `result.frame` at all and `md5` became `None`.

This module is the *one* place that knows how to read what the bench depends on,
and every reader raises [`ReplyShapeError`] naming the tool, the field it wanted
and what it saw.  A reply/tree change therefore fails the phase loudly instead of
producing a silent null sample, and `tools/bench/test_bench_reply.py` pins the
two shapes (v0.2.2 inline full tree, rel-030 compact + `tree_path`).

What the bench needs and where it comes from:

  full tree        `result.tree` when it is a full tree (v0.2.2 and older), else
                   the file named by `result.tree_path` (rel-030 compact replies)
  count text       the full tree's node whose `resource_id` ends `:id/count`
  frame identity   `result.frame.md5` on a launch/reset/observe capture that asked
                   for pixels (`pixels:true`, or a frame-only `acos_observe`)
  screenshot       `result.screenshot.path` (a PNG the daemon wrote under the
                   session's artifacts), never a caller-invented path
  settle           `result.settle` (`mode`, `settled`, `bound`, and the tap phase
                   timings)
  teardown         `result.teardown` (`verified`, `lifecycle`, survivors)

Nothing here weakens a check: a missing field is an error, an empty md5 is an
error, a truncated tree is an error for a screen check, and a compact page is
never mistaken for the full screen.
"""

from __future__ import annotations

import json
from pathlib import Path

#: The counter fixture's ids; a reference screen that is not the counter screen
#: must be refused before the bench times anything against it.
COUNTER_IDS = ("count", "inc", "reset")

#: The counter fixture's package (for the record; the ids are the hard check).
COUNTER_PACKAGE = "dev.acos.fixtures.counter"


class ReplyShapeError(RuntimeError):
    """A reply does not carry the data the bench depends on."""


def _keys(value):
    return sorted(value) if isinstance(value, dict) else type(value).__name__


def field(obj, path, where):
    """`obj[path]` (dotted) or a ReplyShapeError that says what was seen."""
    cur = obj
    for part in path.split("."):
        if not isinstance(cur, dict) or part not in cur:
            raise ReplyShapeError(
                "%s: reply has no `%s` (saw %s)" % (where, path, _keys(cur)))
        cur = cur[part]
    return cur


def typed(obj, path, kind, where):
    value = field(obj, path, where)
    if isinstance(value, bool) and kind is not bool:
        raise ReplyShapeError("%s: `%s` is a bool, expected %s"
                              % (where, path, kind.__name__))
    if not isinstance(value, kind):
        raise ReplyShapeError("%s: `%s` is %s, expected %s"
                              % (where, path, type(value).__name__, kind.__name__))
    return value


def result_of(envelope, tool):
    """The `result` object of a successful reply, or a shape error."""
    if not isinstance(envelope, dict):
        raise ReplyShapeError("%s: no envelope (saw %r)" % (tool, envelope))
    if not envelope.get("ok"):
        error = envelope.get("error") or {}
        raise ReplyShapeError("%s: reply is not ok: %s %s"
                              % (tool, error.get("code"), error.get("message")))
    return typed(envelope, "result", dict, tool)


# --------------------------------------------------------------------- trees
def nodes(tree):
    """Every node of a full ART tree, recursively."""
    out = []

    def walk(items):
        for node in items or []:
            if isinstance(node, dict):
                out.append(node)
                walk(node.get("children"))

    if not isinstance(tree, dict):
        raise ReplyShapeError("tree: expected an object, saw %s"
                              % type(tree).__name__)
    walk(tree.get("nodes"))
    return out


def node_by_id(tree, short_id):
    """Match `pkg:id/name` or the bare `name` (same rule as the CLI selector)."""
    for node in nodes(tree):
        rid = node.get("resource_id") or ""
        if rid == short_id or rid.endswith(":id/" + short_id):
            return node
    return None


def node_text(node):
    if not isinstance(node, dict):
        return None
    value = node.get("text")
    if value is None:
        value = node.get("content_desc")
    return None if value is None else str(value)


def count_text(tree):
    node = node_by_id(tree, "count")
    return node_text(node) if node is not None else None


def is_full_tree(value):
    return isinstance(value, dict) and "nodes" in value and value.get("compact") is not True


def full_tree(result, where):
    """The full tree of a reply: inline (v0.2.2-) or from `tree_path` (rel-030+)."""
    tree = field(result, "tree", where)
    if isinstance(tree, dict) and tree.get("compact") is True:
        path = field(result, "tree_path", where + " (compact tree)")
        if not isinstance(path, str) or not path:
            raise ReplyShapeError(
                "%s: compact tree names no `tree_path` (%r); the bench cannot see "
                "the screen" % (where, path))
        try:
            doc = json.loads(Path(path).read_text())
        except (OSError, ValueError) as exc:
            raise ReplyShapeError(
                "%s: cannot read the full tree at tree_path %s: %s"
                % (where, path, exc)) from exc
        if not is_full_tree(doc):
            raise ReplyShapeError(
                "%s: the file named by tree_path (%s) is not a full tree: %s"
                % (where, path, _keys(doc)))
        return doc
    if is_full_tree(tree):
        return tree
    raise ReplyShapeError(
        "%s: `result.tree` is neither a full tree (needs `nodes`) nor a compact "
        "page with a readable `tree_path`: %s" % (where, _keys(tree)))


def screen_signature(tree):
    """Identity of a screen, independent of list order and of the compact page."""
    return tuple(sorted(
        (n.get("resource_id") or "", n.get("class") or "", n.get("text") or "",
         n.get("content_desc") or "", n.get("bounds") or "")
        for n in nodes(tree)))


def counter_screen_problems(tree, expect_text="0", required_ids=COUNTER_IDS):
    """Reasons `tree` is not the counter fixture's count-N screen (empty = good).

    Deliberately shape-tolerant and not count-tolerant: the ids the fixture lays
    out must be there, the count node's text is exact, the walk must not have been
    truncated, and there is a floor on the node set (a skeleton with only the
    root is not a screen).  This replaces P7's hard-coded `EXPECT_NODES == 12`,
    which was a measurement of one ART revision, not a correctness rule (the
    counter tree is 14 nodes since v0.2.2).
    """
    problems = []
    if not isinstance(tree, dict):
        return ["tree is %s, not an object" % type(tree).__name__]
    if tree.get("truncated") is not False:
        problems.append("tree is truncated/marked %r; a truncated walk cannot "
                        "prove a screen" % (tree.get("truncated"),))
    walked = nodes(tree)
    if len(walked) < 4:
        problems.append("only %d node(s): that is a skeleton, not the counter "
                        "screen" % len(walked))
    for short_id in required_ids:
        if node_by_id(tree, short_id) is None:
            problems.append("no node with resource_id `:id/%s`" % short_id)
    count = node_by_id(tree, "count")
    got = node_text(count) if count is not None else None
    if count is not None and got != expect_text:
        problems.append("count text is %r, expected %r" % (got, expect_text))
    return problems


def require_counter_screen(tree, where, expect_text="0"):
    problems = counter_screen_problems(tree, expect_text=expect_text)
    if problems:
        raise ReplyShapeError("%s: not the counter count-%s screen: %s"
                              % (where, expect_text, "; ".join(problems)))
    return tree


# -------------------------------------------------------------------- frames
def frame(result, where):
    value = field(result, "frame", where)
    if not isinstance(value, dict):
        raise ReplyShapeError("%s: `result.frame` is %s, expected an object"
                              % (where, type(value).__name__))
    return value


def frame_md5(result, where):
    """The capture's pixel md5; empty/missing is an error, never a `None` sample."""
    value = frame(result, where)
    md5 = value.get("md5")
    if not isinstance(md5, str) or not md5:
        raise ReplyShapeError(
            "%s: result.frame.md5 is %r (frame keys %s). A frame the bench "
            "compares must be a pixel capture: ask `pixels:true` on a launch or "
            "use a frame-only `acos_observe`." % (where, md5, _keys(value)))
    return md5


def identical_captures(md5s, where):
    """Two settled captures of one screen must be byte-identical (P9/P7 rule)."""
    bad = [m for m in md5s if not isinstance(m, str) or not m]
    if bad or len(set(md5s)) != 1:
        raise ReplyShapeError(
            "%s: two consecutive settled captures differ or carry no md5: %r"
            % (where, list(md5s)))
    return md5s[0]


# -------------------------------------------------------------------- settle
TAP_SETTLE_FIELDS = ("mode", "settled", "bound")


def settle(result, where):
    value = field(result, "settle", where)
    if not isinstance(value, dict):
        raise ReplyShapeError("%s: `result.settle` is %s, expected an object"
                              % (where, type(value).__name__))
    return value


def tap_settle(result, where):
    """The tap reply's settle block, with the phase fields the bench times.

    The *verdict* is `result.state_changed` (a bool): in strict mode the engine's
    host-side pixel+tree comparison decides it, so `settle.state_changed` is
    deliberately `null` there (`SettleTiming` in acos-art).  The M8 gate used to
    read `settle.get("state_changed") is not False`, which is true for `None` —
    i.e. it never checked anything, and a renamed field would not have changed
    that.  Read the top-level field and require it to be a bool: a `False`
    verdict is still a legitimate failed tap, not a shape error.
    """
    value = settle(result, where)
    missing = [key for key in TAP_SETTLE_FIELDS if key not in value]
    if missing:
        raise ReplyShapeError("%s: settle is missing %s (saw %s)"
                              % (where, missing, _keys(value)))
    if not isinstance(result.get("state_changed"), bool):
        raise ReplyShapeError(
            "%s: `result.state_changed` is %r, expected a bool (the tap verdict "
            "lives at the top level; settle.state_changed is null in strict "
            "mode)" % (where, result.get("state_changed")))
    if not isinstance(value.get("ack_ms"), (int, float)) or isinstance(
            value.get("ack_ms"), bool):
        raise ReplyShapeError("%s: settle.ack_ms is %r, expected a number (keys %s)"
                              % (where, value.get("ack_ms"), _keys(value)))
    if value.get("frame_ms") is None and value.get("first_change_ms") is None:
        raise ReplyShapeError(
            "%s: settle has neither `frame_ms` (strict) nor `first_change_ms` "
            "(fast); the step's phase timing is gone (keys %s)"
            % (where, _keys(value)))
    return value


# ------------------------------------------------------------------ verb API
def require_launch(envelope, where, pixels=True):
    """A launch reply the bench can measure: full tree, settle, pixel frame."""
    res = result_of(envelope, where)
    if res.get("launched") is not True:
        raise ReplyShapeError("%s: `launched` is %r, expected true"
                              % (where, res.get("launched")))
    tree = full_tree(res, where)
    settle(res, where)
    if pixels:
        frame_md5(res, where)
    return res, tree


def require_reset(envelope, where):
    res = result_of(envelope, where)
    for key in ("state_reset", "data_cleared"):
        if res.get(key) is not True:
            raise ReplyShapeError("%s: `%s` is %r, expected true"
                                  % (where, key, res.get(key)))
    tree = full_tree(res, where)
    typed(res, "frame_seq", int, where)
    settle(res, where)
    return res, tree


def require_tree_read(envelope, where):
    res = result_of(envelope, where)
    tree = full_tree(res, where)
    typed(res, "node_count", int, where)
    settle(res, where)
    return res, tree


def require_tap(envelope, where):
    res = result_of(envelope, where)
    return res, tap_settle(res, where)


def require_capture(envelope, where):
    """A frame-only `acos_observe` reply: a fresh host capture with its md5."""
    res = result_of(envelope, where)
    md5 = frame_md5(res, where)
    typed(res, "frame.frame_seq", int, where)
    return res, md5


def screenshot_path(result, where):
    """The PNG the daemon wrote for `screenshot:true` (never a caller path)."""
    value = typed(result, "screenshot", dict, where)
    path = value.get("path")
    if not isinstance(path, str) or not path:
        raise ReplyShapeError("%s: `result.screenshot.path` is %r (keys %s)"
                              % (where, path, _keys(value)))
    return Path(path), value


def require_destroy(envelope, where):
    res = result_of(envelope, where)
    teardown = typed(res, "teardown", dict, where)
    if "verified" not in teardown:
        raise ReplyShapeError("%s: teardown has no `verified` (keys %s)"
                              % (where, _keys(teardown)))
    return res, teardown
