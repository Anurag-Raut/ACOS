#!/usr/bin/env python3
"""PARITYFIX F2: every tree's `visible_bounds` is clipped to the display it was
captured on, including after a rotation.

    tools/tests/test-tree-display-clip.py <verify artifact dir> [--need-landscape]

The directory is the output of `acos verify scenarios/rot1-rotate.yaml --engine
art`. For each `tree/*.json`:

  * the display is the application window's bounds in that very tree (not the
    scenario's display, not a cached value);
  * every node with a `visible_bounds` has exactly its `bounds` clipped to that
    display (tools/parity/parity_nodes.py:tree_inconsistencies, the check
    PARITY1 used to find the bug).

`--need-landscape` also requires at least one tree whose display is wider than
tall *and* a node whose visible right edge is beyond the portrait width: a run
that never rotated, or whose clip still stops at the old width, cannot pass.
"""
import glob
import json
import os
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, os.path.join(HERE, "..", "parity"))
import parity_nodes as pn  # noqa: E402


def main(argv):
    args = [a for a in argv[1:] if not a.startswith("--")]
    need_landscape = "--need-landscape" in argv
    if len(args) != 1:
        print(__doc__)
        return 2
    trees = sorted(glob.glob(os.path.join(args[0], "tree", "*.json")),
                   key=lambda p: int(os.path.basename(p).split(".")[0]))
    if not trees:
        print(f"no tree/*.json under {args[0]}")
        return 2
    bad = 0
    landscape = 0
    wide_node = 0
    for path in trees:
        doc = json.load(open(path, encoding="utf-8"))
        display = pn.tree_display(doc)
        nodes = pn.acos_nodes(doc)
        issues = pn.tree_inconsistencies([pn.with_dp(dict(n), 420) for n in nodes],
                                         display)
        if display and display[2] - display[0] > display[3] - display[1]:
            landscape += 1
            wide_node += sum(1 for n in nodes
                             if (n.get("visible_bounds_px") or [0, 0, 0, 0])[2] > 1080)
        name = os.path.basename(path)
        if issues:
            bad += 1
            first = issues[0]["acos"]
            print(f"FAIL {name}: display {display}, {len(issues)} nodes, e.g. "
                  f"{first['bounds_px']} visible {first['visible_bounds_px']} "
                  f"want {first['expected_visible_bounds_px']}")
        else:
            print(f"PASS {name}: display {display}, {len(nodes)} nodes")
    if need_landscape and not (landscape and wide_node):
        print(f"FAIL no landscape tree with a node visible past x=1080 "
              f"(landscape trees {landscape}, wide nodes {wide_node})")
        bad += 1
    print(f"{len(trees) - bad}/{len(trees)} trees consistent")
    return 1 if bad else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
