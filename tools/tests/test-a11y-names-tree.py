#!/usr/bin/env python3
"""PARITYFIX F1: pin the per-field accessible names of a11y-names-probe.

    tools/tests/test-a11y-names-tree.py <verify artifact dir>

The directory is the output of `acos verify scenarios/a11y-names.yaml
--engine art` (it holds tree/<frame>.json). The expected values are what
AccessibilityNodeInfo reports for each node (AOSP API 34 TextView/View code):

  text         = getText(), else the hint for a TextView/EditText
  hint         = getHintText()
  content_desc = getContentDescription() of the NODE INFO (a delegate-composed
                 description counts, View.getContentDescription() alone is not
                 the reference)

and, in particular, ACOS never publishes a name under an attribute the platform
does not (a label as text, a placeholder as content-desc).
"""
import glob
import json
import os
import sys

EXPECTED = {
    # id: (text, hint, content_desc, password)
    "token": ("Paste your token", "Paste your token", "Personal access token", True),
    "search": ("Search words", "Search words", "Search dictionary", False),
    "recent": ("Recently looked up", "", "", False),
    "both": ("Connect", "", "Connect WaniKani", False),
    "card": ("", "", "Open card", False),
    "card_face": ("card face", "", "", False),
    "delegate_tab": ("Analytics", "", "Analytics, tab 2 of 4", False),
}


def walk(nodes):
    for n in nodes:
        yield n
        yield from walk(n.get("children") or [])


def main(argv):
    if len(argv) != 2:
        print(__doc__)
        return 2
    trees = sorted(glob.glob(os.path.join(argv[1], "tree", "*.json")),
                   key=lambda p: int(os.path.basename(p).split(".")[0]))
    if not trees:
        print(f"no tree/*.json under {argv[1]}")
        return 2
    doc = json.load(open(trees[-1], encoding="utf-8"))
    by_id = {}
    for n in walk(doc["nodes"]):
        rid = n.get("resource_id") or ""
        if rid.startswith("dev.acos.fixtures.a11ynames:id/"):
            by_id[rid.split("/", 1)[1]] = n
    failures = 0
    for name, (text, hint, desc, password) in EXPECTED.items():
        n = by_id.get(name)
        if n is None:
            print(f"FAIL {name}: node not in the tree")
            failures += 1
            continue
        got = (n.get("text") or "", n.get("hint") or "",
               n.get("content_desc") or "", bool(n.get("password")))
        want = (text, hint, desc, password)
        if got != want:
            print(f"FAIL {name}: (text, hint, content_desc, password) = {got!r}, want {want!r}")
            failures += 1
        else:
            print(f"PASS {name}: {got!r}")
    print(f"{len(EXPECTED) - failures}/{len(EXPECTED)} nodes match AccessibilityNodeInfo")
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
