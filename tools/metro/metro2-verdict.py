#!/usr/bin/env python3
"""METRO2c verdict for one dev-loop run directory (metro1-dev-loop.sh output).

Reads observe-before/after.json + the session logcat and prints one JSON line:
  {reload: {mode, context, tree_nodes_before, tree_nodes_after, new_text_copies,
            old_text_copies, stale_roots, js_contexts_started, explicit_id_errors}, pass}
A run that leaves the old text, a doubled tree, or an `explicit id` root-view
warning is a NAMED FAILURE, never a pass (METRO_PLAN.md, METRO2)."""
import json, re, sys

run, logcat = sys.argv[1], sys.argv[2]
NEW, OLD = "Hello Metro (reloaded)", "Hello ACOS"

def obs(name):
    r = json.load(open(f"{run}/{name}.json"))["result"]
    return r["node_count"], [n.get("text", "") for n in r["tree"]["nodes"] if "text" in n]

nb, tb = obs("observe-before")
na, ta = obs("observe-after")
log = open(logcat, errors="replace").read()
contexts = len(re.findall(r'ReactNativeJS: Running "rnhello"', log))
explicit = len(re.findall(r"explicit id", log))
destroys = len(re.findall(r"CatalystInstanceImpl\.destroy\(\) start", log))
new_copies = sum(t == NEW for t in ta)
old_copies = sum(t == OLD for t in ta)
# a stale root shows up as a tree that grew by a whole copy of the before-tree
stale = 1 if na >= 2 * nb - 3 and na > nb else 0
mode = "reload" if destroys else "fast-refresh"
ok = (new_copies == 1 and old_copies == 0 and not stale and explicit == 0
      and na <= nb and contexts == (1 + destroys))
print(json.dumps({"reload": {
    "mode": mode, "context": "new" if destroys else "same",
    "tree_nodes_before": nb, "tree_nodes_after": na,
    "new_text_copies": new_copies, "old_text_copies": old_copies,
    "stale_roots": stale, "js_contexts_started": contexts,
    "catalyst_destroys": destroys, "explicit_id_errors": explicit},
    "pass": ok}))
sys.exit(0 if ok else 1)
