#!/usr/bin/env python3
"""MPROC4 driver: the second guest process, for real.

Drives one interactive `acos-art launch` session of `proc-a` with `proc-b`
installed beside it (`--extra-apk`) and proves, from replies the session itself
produced:

  1. before any cross-package call there is **one** process (tag 0, proc-a);
  2. tapping "Start B" (`startActivityForResult` to proc-b's activity) makes the
     launcher spawn a **second** guest: a new tag with a pid, proc-b's package;
  3. the window on top is proc-b's (probe tree + log lines from proc-b's own
     process), i.e. proc-a is paused/stopped under it;
  4. proc-b answers with `setResult` + `finish` and proc-a's `onActivityResult`
     runs (Back from B returns to A);
  5. "Bind B" binds proc-b's service and one `transact` round trip comes back
     from proc-b's process (cross-process binder through the peer's nodes);
  6. the activity manager's process list carries both processes at the end.

Every fact printed is a reply line from the session (or a log entry the guest
logged); nothing is inferred.

    python3 tools/mproc4-two-process-drive.py <acos-art> <proc-a.apk> <proc-b.apk> <out-dir>
"""

import json
import os
import subprocess
import sys
import time

art, apk_a, apk_b, out = sys.argv[1], sys.argv[2], sys.argv[3], sys.argv[4]
env = dict(os.environ)
env["ACOSLAUNCH_INTERACTIVE"] = "1"
env.pop("ACOS_HOME", None)
os.makedirs(out, exist_ok=True)

proc = subprocess.Popen(
    [
        art,
        "launch",
        "--apk",
        apk_a,
        "--extra-apk",
        apk_b,
        "--until",
        "activity-resumed",
        "--out",
        out,
    ],
    stdin=subprocess.PIPE,
    stdout=subprocess.PIPE,
    stderr=open(os.path.join(out, "launcher.stderr.log"), "w"),
    text=True,
    env=env,
)

facts = {}
failures = []
# MPROC4 is committed in steps: step 3b owns the spawn + per-tag process list,
# step 3c owns the cross-process UI flow (pause/stop delivery, the top window,
# the activity result, the service round trip). Both are reported; only the 3b
# set decides this driver's exit status, so a 3c gap is never hidden behind a
# failing exit code (and never counted as a pass either).
STEP3B = {
    "ready",
    "no-second-process-before",
    "tap-start-b",
    "second-process-spawned",
    "second-process-is-proc-b",
    "proc-b-resumed-in-its-own-process",
    "process-table-still-has-b",
    # MPROC5 step 1: the top-of-task rule. These were OPEN_3C while nothing
    # paused proc-a; the host now delivers PauseActivityItem+StopActivityItem to
    # A's own IApplicationThread and the served tree comes from the top
    # process's probe sample, so they are part of the gate.
    "proc-b-window-on-top",
    "proc-a-paused-or-stopped-under-b",
    "back-from-b-returns-to-a",
}


def reply():
    line = proc.stdout.readline()
    if not line:
        return None
    try:
        return json.loads(line)
    except json.JSONDecodeError:
        print("RAW", line[:300])
        return None


def send(request):
    proc.stdin.write(json.dumps(request) + "\n")
    proc.stdin.flush()
    return reply()


def want(condition, name, detail):
    facts[name] = {"ok": bool(condition), "detail": detail, "step": "3b" if name in STEP3B else "3c"}
    if not condition and name in STEP3B:
        failures.append(name)
    print(("PASS " if condition else "FAIL ") + name + ": " + json.dumps(detail)[:400])


def find_node(tree, text):
    """Depth-first search for a node whose text/description starts with `text`."""
    if isinstance(tree, dict):
        label = tree.get("text") or tree.get("content_description") or ""
        if isinstance(label, str) and label.strip().startswith(text):
            bounds = tree.get("bounds_on_screen") or tree.get("bounds") or tree.get("rect")
            return bounds
        for value in tree.values():
            found = find_node(value, text)
            if found is not None:
                return found
    elif isinstance(tree, list):
        for value in tree:
            found = find_node(value, text)
            if found is not None:
                return found
    return None


def tap_text(text, tree_reply=None):
    tree = tree_reply or send({"cmd": "tree", "pixels": False, "settle": "fast"})
    bounds = find_node(tree.get("tree") if tree else None, text)
    if not bounds:
        return None, tree
    if isinstance(bounds, dict):
        left, top = bounds.get("left", 0), bounds.get("top", 0)
        right, bottom = bounds.get("right", 0), bounds.get("bottom", 0)
    elif isinstance(bounds, str):
        # The probe writes `[left,top][right,bottom]` (display space).
        import re

        nums = [int(n) for n in re.findall(r"-?\d+", bounds)]
        if len(nums) < 4:
            return None, tree_reply
        left, top, right, bottom = nums[:4]
    else:
        left, top, right, bottom = bounds[:4]
    return send(
        {
            "cmd": "tap",
            "x": int((left + right) // 2),
            "y": int((top + bottom) // 2),
        }
    ), tree


def probe_lines(entries):
    return [
        e.get("message", "")
        for e in entries
        if isinstance(e, dict) and "ACOSPROBE" in str(e.get("message", ""))
    ]


ready = reply() or {}
want(ready.get("ok") is True, "ready", {"package": ready.get("package")})

before = send({"cmd": "ctl", "module": "processes", "op": "list"}) or {}
want(
    before.get("ok") is True and len(before.get("processes") or []) == 0,
    "no-second-process-before",
    {"processes": before.get("processes"), "requests": before.get("requests")},
)

start_reply, tree = tap_text("Start B")
want(start_reply is not None and start_reply.get("ok") is True, "tap-start-b", start_reply)
want(
    find_node(tree.get("tree") if tree else None, "Start B") is not None,
    "start-b-button-in-tree",
    {"tree_present": bool(tree)},
)

# The spawn is asynchronous (the peer hands the request over, the launcher starts
# the guest and the guest boots ART); poll the launcher's own table.
after = None
deadline = time.time() + 45
while time.time() < deadline:
    after = send({"cmd": "ctl", "module": "processes", "op": "list"}) or {}
    if after.get("processes"):
        break
    time.sleep(0.5)

second = (after or {}).get("processes") or []
want(
    len(second) == 1 and second[0].get("tag") == 1,
    "second-process-spawned",
    {"processes": second, "requests": (after or {}).get("requests")},
)
want(
    second and second[0].get("package") == "dev.acos.fixtures.procb",
    "second-process-is-proc-b",
    second,
)

# Give B's guest time to attach, bind and resume, then look at what is on top.
tree_b = None
deadline = time.time() + 25
while time.time() < deadline:
    tree_b = send({"cmd": "tree", "pixels": False, "settle": "fast"}) or {}
    if find_node(tree_b.get("tree"), "Finish B") is not None:
        break
    time.sleep(0.5)
want(
    find_node((tree_b or {}).get("tree"), "Finish B") is not None,
    "proc-b-window-on-top",
    {"probe_seq": (tree_b or {}).get("probe_seq")},
)

logs = send({"cmd": "logs"}) or {}
lines = probe_lines(logs.get("entries") or logs.get("logs") or [])
want(
    any(line.startswith("ACOSPROBE b resumed") for line in lines),
    "proc-b-resumed-in-its-own-process",
    {"lines": [l for l in lines if l.startswith("ACOSPROBE b")][:8]},
)
want(
    any(line.startswith("ACOSPROBE a paused") or line.startswith("ACOSPROBE a stopped")
        for line in lines),
    "proc-a-paused-or-stopped-under-b",
    {"lines": [l for l in lines if l.startswith("ACOSPROBE a")][:12]},
)

# B: setResult + finish -> A's onActivityResult (Back from B returns to A).
finish_reply, _ = tap_text("Finish B", tree_b)
want(finish_reply is not None and finish_reply.get("ok") is True, "tap-finish-b", finish_reply)
result = None
deadline = time.time() + 10
while time.time() < deadline:
    logs = send({"cmd": "logs"}) or {}
    lines = probe_lines(logs.get("entries") or logs.get("logs") or [])
    result = next((l for l in lines if l.startswith("ACOSPROBE a result")), None)
    if result:
        break
    time.sleep(0.5)
want(
    result is not None and "data=from-b" in result,
    "proc-a-onActivityResult-from-b",
    {"line": result},
)

back = send({"cmd": "tree", "pixels": False, "settle": "fast"}) or {}
want(
    find_node(back.get("tree"), "Start B") is not None,
    "back-from-b-returns-to-a",
    {"probe_seq": back.get("probe_seq")},
)

# Cross-process binder: bind B's service and do one transaction.
bind_reply, _ = tap_text("Bind B", back)
want(bind_reply is not None and bind_reply.get("ok") is True, "tap-bind-b", bind_reply)
transact = None
deadline = time.time() + 10
while time.time() < deadline:
    logs = send({"cmd": "logs"}) or {}
    lines = probe_lines(logs.get("entries") or logs.get("logs") or [])
    transact = next((l for l in lines if l.startswith("ACOSPROBE a transact")), None)
    if transact:
        break
    time.sleep(0.5)
want(
    transact is not None and "out=echo:ping" in transact,
    "aidl-round-trip-through-foreign-node",
    {"line": transact},
)
want(
    any(line.startswith("ACOSPROBE b transact") for line in lines),
    "transaction-ran-in-proc-b",
    {"lines": [l for l in lines if l.startswith("ACOSPROBE b transact")][:4]},
)

final = send({"cmd": "ctl", "module": "processes", "op": "list"}) or {}
want(
    len(final.get("processes") or []) == 1,
    "process-table-still-has-b",
    {"processes": final.get("processes")},
)

send({"cmd": "close"})
try:
    proc.wait(timeout=90)
except subprocess.TimeoutExpired:
    proc.kill()
    proc.wait()
print("EXIT", proc.returncode)
print("FACTS", json.dumps(facts, indent=2)[:4000])
step3c = [name for name, fact in facts.items() if fact["step"] == "3c" and not fact["ok"]]
print("FAILURES_3B", json.dumps(failures))
print("OPEN_3C", json.dumps(step3c))
sys.exit(0 if not failures else 1)
