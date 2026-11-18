#!/usr/bin/env python3
"""Fail-closed tests for the PARITY1 UI-tree comparator.

Run: python3 tools/tests/test-parity-harness.py

Pure: no phone, no ART, no network. The real captures under
`fixtures/reference-device/` are used where a parser needs real platform XML.
"""
import json
import os
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.dirname(os.path.dirname(HERE))
sys.path.insert(0, os.path.join(REPO, "tools/parity"))

import parity_nodes as pn  # noqa: E402

PASSES = FAILURES = 0


def check(name, cond, detail=""):
    global PASSES, FAILURES
    if cond:
        PASSES += 1
        print(f"PASS {name}")
    else:
        FAILURES += 1
        print(f"FAIL {name}" + (f": {detail}" if detail else ""))


def acos_tree(nodes):
    return {"nodes": nodes}


def node(resource_id="", cls="android.widget.FrameLayout", text="", desc="",
         bounds=(0, 0, 0, 0), visible=True, enabled=True, checked=False,
         focused=False, children=None):
    return {
        "id": "n_" + (resource_id or cls),
        "resource_id": resource_id,
        "class": cls,
        "text": text,
        "content_desc": desc,
        "bounds": list(bounds),
        "visible": visible,
        "enabled": enabled,
        "checked": checked,
        "focused": focused,
        "children": children or [],
    }


def pair_nodes(a, p, tol=1.5, shift=None):
    a = pn.with_dp(dict(a), 420)
    p = pn.with_dp(dict(p), 400)
    matched, missing, extra, _ = pn.match_nodes([a], [p])
    assert matched or (missing and extra), "nothing matched"
    diffs = []
    for an, pn_node, why in matched:
        diffs.extend(pn.diff_pair(an, pn_node, why, tol_dp=tol,
                                  decor_shift=shift))
    return diffs


# --- 1. dp normalisation ---------------------------------------------------
# The same 42 dp layout at the two densities differs by 5% in pixels and must
# compare equal in dp.
a = pn.make_node(resource_id="x:id/inc", class_name="android.widget.Button",
                 text="Increment", bounds=[42, 368, 273, 494])
p = pn.make_node(resource_id="x:id/inc", class_name="android.widget.Button",
                 text="Increment", bounds=[40, 350, 260, 470])
check("dp normalisation compares the same dp layout equal",
      pair_nodes(a, p) == [], pair_nodes(a, p))

# 10 dp of real movement must still be reported.
p2 = pn.make_node(resource_id="x:id/inc", class_name="android.widget.Button",
                  text="Increment", bounds=[40, 376, 260, 496])
diffs = pair_nodes(a, p2)
check("a 10 dp move is reported as bounds_changed",
      len(diffs) == 1 and diffs[0]["kind"] == "bounds_changed", diffs)

# --- 2. keys and matching --------------------------------------------------
nodes = pn.acos_nodes(acos_tree([
    node(cls="com.android.internal.policy.DecorView", bounds=(0, 0, 1080, 2400),
         children=[
             node("android:id/content", bounds=(0, 248, 1080, 2400), children=[
                 node(cls="android.widget.LinearLayout", bounds=(0, 248, 1080, 2400),
                      children=[
                          node("x:id/count", "android.widget.TextView", "0",
                               bounds=(40, 288, 61, 334)),
                      ]),
             ]),
         ]),
]))
check("ACOS tree parsing keeps the real resource ids",
      [n["id"] for n in nodes if n["id"]] == ["android:id/content", "x:id/count"],
      [n["id"] for n in nodes])
check("ACOS internal node handles are never used as resource ids",
      all(not n["id"].startswith("n_") for n in nodes),
      [n["id"] for n in nodes])

# A phone that exposes the same views without the ACOS-only framework node.
phone_nodes = pn.annotate_tree([
    pn.make_node(class_name="android.widget.FrameLayout", bounds=[0, 0, 1080, 2400],
                 depth=0),
    pn.make_node(resource_id="android:id/content", class_name="android.widget.FrameLayout",
                 bounds=[0, 248, 1080, 2400], depth=1),
    pn.make_node(class_name="android.widget.LinearLayout", bounds=[0, 248, 1080, 2400],
                 depth=2),
    pn.make_node(resource_id="x:id/count", class_name="android.widget.TextView",
                 text="0", bounds=[40, 288, 61, 334], depth=3),
])
acos_nodes = pn.annotate_tree([pn.with_dp(dict(n), 420) for n in nodes])
phone_nodes = [pn.with_dp(dict(n), 400) for n in phone_nodes]
matched, missing, extra, stats = pn.match_nodes(acos_nodes, phone_nodes)
check("containers pair by tree position even with an absent framework node",
      len(matched) == 4, stats)

# --- 3. text vs content-desc ----------------------------------------------
a = pn.with_dp(pn.make_node(resource_id="x:id/x", class_name="android.widget.TextView",
                            text="Not connected", desc=""), 420)
p = pn.with_dp(pn.make_node(resource_id="x:id/x", class_name="android.widget.TextView",
                            text="", desc="Not connected"), 400)
d = pn.diff_pair(a, p, "id")
check("a name that moved between text and content-desc is its own kind",
      [x["kind"] for x in d] == ["text_vs_desc"], d)

# --- 4. state and visibility ----------------------------------------------
a = pn.with_dp(pn.make_node(resource_id="x:id/b", class_name="android.widget.Button",
                            text="Save", enabled=False, checked=False,
                            focused=False, visible=True, bounds=[0, 0, 10, 10]), 420)
p = pn.with_dp(pn.make_node(resource_id="x:id/b", class_name="android.widget.Button",
                            text="Save", enabled=True, checked=True,
                            focused=True, visible=False, bounds=[0, 0, 10, 10]), 400)
kinds = sorted({x["kind"] for x in pn.diff_pair(a, p, "id")})
check("enabled/checked/focused/visible are compared",
      kinds == ["state_changed", "visibility_changed"], kinds)

# --- 5. allowlist scoping --------------------------------------------------
entry = {"id": "scope", "match": {"kind": "extra_in_acos",
                                 "node": "id:android:id/action_bar",
                                 "phone_source": "recorded-*"},
         "reason": "test"}
diff = {"kind": "extra_in_acos", "key": "id:android:id/action_bar"}
check("allowlist entry matches its phone source",
      pn.allowlist_match(entry, diff, "counter", "recorded-snapshot"))
check("allowlist entry does not leak into a live run",
      not pn.allowlist_match(entry, diff, "counter", "live"))
check("allowlist entry does not leak into another node",
      not pn.allowlist_match(entry, {"kind": "extra_in_acos",
                                     "key": "id:android:id/action_mode_bar"},
                             "counter", "recorded-snapshot"))

# --- 6. window-frame shift -------------------------------------------------
a = pn.with_dp(pn.make_node(resource_id="android:id/content",
                            class_name="android.widget.FrameLayout",
                            bounds=[0, 275, 1080, 2337]), 420)
p = pn.with_dp(pn.make_node(resource_id="android:id/content",
                            class_name="android.widget.FrameLayout",
                            bounds=[0, 248, 1080, 2400]), 400)
shift = pn.decor_shift([(a, p, "id")])
check("the frame shift is measured from the content node's top-left edge",
      shift == [0.0, -5.56], shift)
inner_a = pn.with_dp(pn.make_node(resource_id="x:id/count",
                                  class_name="android.widget.TextView",
                                  bounds=[42, 317, 63, 368]), 420)
inner_p = pn.with_dp(pn.make_node(resource_id="x:id/count",
                                  class_name="android.widget.TextView",
                                  bounds=[40, 288, 61, 334]), 400)
diffs = pn.diff_pair(inner_a, inner_p, "id", decor_shift=shift,
                     display_a=[0, 0, 1080, 2400], display_p=[0, 0, 1080, 2400])
check("a size-preserving translation is explained by the frame shift",
      len(diffs) == 1 and diffs[0].get("explained_by") == "decor_shift", diffs)
edge = pn.diff_pair(a, p, "id", decor_shift=shift,
                    display_a=[0, 0, 1080, 2400], display_p=[0, 0, 1080, 2400])
check("an edge-to-edge window height is its own explanation",
      len(edge) == 1 and edge[0].get("explained_by") == "edge_to_edge", edge)

# --- 7. ACOS's own tree consistency (the rotation bug) --------------------
rotation = pn.acos_nodes({"nodes": [
    {"id": "n0_", "resource_id": "", "class": "com.android.internal.policy.DecorView",
     "text": "", "content_desc": "", "bounds": [0, 0, 2400, 1080],
     "visible_bounds": [0, 0, 1080, 1080], "visible": True, "children": []},
], "windows": [{"kind": "application", "bounds": [0, 0, 2400, 1080]}]})
display = pn.tree_display({"nodes": [{"bounds": [0, 0, 2400, 1080]}],
                           "windows": [{"kind": "application",
                                        "bounds": [0, 0, 2400, 1080]}]})
bad = pn.tree_inconsistencies(rotation, display)
check("visible_bounds clipped to a stale display is reported",
      len(bad) == 1 and bad[0]["kind"] == "acos_tree_inconsistent", bad)
ok = pn.tree_inconsistencies([pn.make_node(bounds=[0, 0, 2400, 1080])], display)
check("a consistent visible_bounds is not reported", ok == [])

# --- 8. uiautomator XML + package filtering -------------------------------
xml = ("<?xml version='1.0'?><hierarchy rotation=\"0\">"
       "<node index=\"0\" text=\"\" resource-id=\"com.android.systemui:id/clock\""
       " class=\"android.widget.TextView\" package=\"com.android.systemui\""
       " content-desc=\"\" enabled=\"true\" focused=\"false\" selected=\"false\""
       " bounds=\"[45,33][128,75]\">"
       "<node index=\"0\" text=\"\" resource-id=\"\" class=\"android.widget.FrameLayout\""
       " package=\"dev.acos.fixtures.counter\" content-desc=\"\" enabled=\"true\""
       " focused=\"false\" bounds=\"[0,0][1080,2400]\">"
       "<node index=\"0\" text=\"Increment\" resource-id=\"dev.acos.fixtures.counter:id/inc\""
       " class=\"android.widget.Button\" package=\"dev.acos.fixtures.counter\""
       " content-desc=\"\" enabled=\"true\" focused=\"false\" bounds=\"[40,334][262,454]\"/>"
       "</node></node></hierarchy>")
parsed = pn.phone_nodes_from_uiautomator(xml)
check("uiautomator XML parses into nodes with depth", len(parsed) == 3, parsed)
kept, dropped = pn.filter_package(parsed, "dev.acos.fixtures.counter")
check("another package's window is dropped and counted",
      len(kept) == 2 and dropped == 1, (len(kept), dropped))
check("a zero-size node is not visible",
      pn.phone_nodes_from_uiautomator(
          "<hierarchy><node text=\"\" resource-id=\"\" class=\"android.widget.Space\""
          " package=\"p\" bounds=\"[7,7][7,7]\"/></hierarchy>")[0]["visible"] is False)

# --- 9. real recorded capture ---------------------------------------------
capture = os.path.join(REPO, "fixtures/reference-device/counter",
                       "snapshot-01-wait_for:-id-:-count.json")
if os.path.exists(capture):
    doc = json.load(open(capture))
    nodes, _ = pn.filter_package(pn.phone_nodes_from_snapshot(doc),
                                 "dev.acos.fixtures.counter")
    by_id = {n["id"]: n for n in nodes}
    check("the recorded phone capture yields the app's own ids",
          "dev.acos.fixtures.counter:id/inc" in by_id,
          sorted(by_id))
    check("the recorded label is read as the node text (TREE1)",
          by_id["dev.acos.fixtures.counter:id/count"]["text"] == "0",
          by_id["dev.acos.fixtures.counter:id/count"]["text"])
    check("systemui chrome is not part of the app window",
          all(not n["id"].startswith("com.android.systemui") for n in nodes))

# --- 10. scenario step alignment -----------------------------------------
sys.path.insert(0, os.path.join(REPO, "tools/parity"))
import diff_trees  # noqa: E402

art = os.path.join(REPO, ".cache/parity1/runs/counter")
if os.path.exists(os.path.join(art, "verdict.json")):
    side = diff_trees.AcosSide(art)
    seq, tree, source = side.tree_for_step(0)  # launch: no bound frame
    check("a step without a bound frame borrows the next tree of its epoch",
          seq is not None and source == "next_step", (seq, source))
    seq, tree, source = side.tree_for_step(6)  # tap
    check("an action step uses its own bound frame",
          source == "own", (seq, source))

print()
print(f"{PASSES} passed, {FAILURES} failed")
sys.exit(1 if FAILURES else 0)
