#!/usr/bin/env python3
"""MPROC5c step 2 driver: the idle kill, for real.

Drives one interactive `acos-art launch` session of `proc-a` with `proc-b`
installed beside it (`--extra-apk`) and a **short** idle-kill timeout, and
proves from the session's own replies:

  1. before Start B there is no second process (the process table is empty);
  2. Start B spawns proc-b lazily (tag 1, its own pid) and its window is on top;
  3. finishing B returns to proc-a, and B then holds nothing (no started
     activity, no bound service, no visible window);
  4. after the idle timeout the launcher kills it: a `killed` record with
     `reason=idle` and `idle_ms >= timeout`, the process table is empty again
     (the `ProcessRecord` went with it), and the peer closed its
     `IWindowSession` (launcher stderr: `window session N closed`);
  5. the next intent respawns it: a second spawn with a **new pid**, and B's own
     probe logs `created` a second time (a new process, not a resurrected one).

Every fact printed is a reply line from the session (or a log entry the guest /
launcher produced); nothing is inferred.

    python3 tools/mproc5c-idle-kill-drive.py <acos-art> <proc-a.apk> <proc-b.apk> <out-dir> [idle_secs]
"""

import json
import os
import re
import subprocess
import sys
import time

art, apk_a, apk_b, out = sys.argv[1], sys.argv[2], sys.argv[3], sys.argv[4]
idle_secs = int(sys.argv[5]) if len(sys.argv) > 5 else 3
env = dict(os.environ)
env["ACOSLAUNCH_INTERACTIVE"] = "1"
env["ACOSLAUNCH_IDLE_KILL_SECS"] = str(idle_secs)
env.pop("ACOS_HOME", None)
os.makedirs(out, exist_ok=True)

stderr_path = os.path.join(out, "launcher.stderr.log")
stderr_file = open(stderr_path, "w")
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
        "--idle-kill-seconds",
        str(idle_secs),
        "--out",
        out,
    ],
    stdin=subprocess.PIPE,
    stdout=subprocess.PIPE,
    stderr=stderr_file,
    text=True,
    env=env,
)

facts = {}
failures = []


def reply():
    line = proc.stdout.readline()
    if not line:
        return None
    try:
        return json.loads(line)
    except json.JSONDecodeError:
        print("RAW", line[:200])
        return None


def send(request):
    proc.stdin.write(json.dumps(request) + "\n")
    proc.stdin.flush()
    return reply()


def want(condition, name, detail):
    facts[name] = {"ok": bool(condition), "detail": detail}
    if not condition:
        failures.append(name)
    print(("PASS " if condition else "FAIL ") + name + ": " + json.dumps(detail)[:600])


def find_node(tree, text):
    if isinstance(tree, dict):
        label = tree.get("text") or tree.get("content_description") or ""
        if isinstance(label, str) and label.strip().startswith(text):
            return tree.get("bounds_on_screen") or tree.get("bounds") or tree.get("rect")
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
        nums = [int(n) for n in re.findall(r"-?\d+", bounds)]
        if len(nums) < 4:
            return None, tree_reply
        left, top, right, bottom = nums[:4]
    else:
        left, top, right, bottom = bounds[:4]
    return send({"cmd": "tap", "x": int((left + right) // 2), "y": int((top + bottom) // 2)}), tree


def ctl_processes():
    return send({"cmd": "ctl", "module": "processes", "op": "list"}) or {}


def probe_lines(kind):
    logs = send({"cmd": "logs"}) or {}
    entries = logs.get("entries") or logs.get("logs") or []
    return [
        e.get("message", "")
        for e in entries
        if isinstance(e, dict) and str(e.get("message", "")).startswith("ACOSPROBE " + kind)
    ]


def wait_for_text(text, seconds):
    deadline = time.time() + seconds
    last = None
    while time.time() < deadline:
        last = send({"cmd": "tree", "pixels": False, "settle": "fast"}) or {}
        if find_node(last.get("tree"), text) is not None:
            return True, last
        time.sleep(0.4)
    return False, last


ready = reply() or {}
want(ready.get("ok") is True, "ready", {"package": ready.get("package")})

before = ctl_processes()
want(
    before.get("ok") is True and len(before.get("processes") or []) == 0,
    "no-second-process-before",
    {"processes": before.get("processes")},
)

start_reply, _ = tap_text("Start B")
want(start_reply is not None and start_reply.get("ok") is True, "tap-start-b", start_reply)

spawned = None
deadline = time.time() + 45
while time.time() < deadline:
    after = ctl_processes()
    if after.get("processes"):
        spawned = after
        break
    time.sleep(0.5)
spawns = (spawned or {}).get("processes") or []
want(
    len(spawns) == 1 and spawns[0].get("tag") == 1 and spawns[0].get("alive") is True,
    "second-process-spawned",
    {"processes": spawns},
)
first_pid = spawns[0]["pid"] if spawns else None

on_top, tree_b = wait_for_text("Finish B", 30)
want(on_top, "proc-b-window-on-top", {"probe_seq": (tree_b or {}).get("probe_seq")})

finish_reply, _ = tap_text("Finish B", tree_b)
want(finish_reply is not None and finish_reply.get("ok") is True, "tap-finish-b", finish_reply)

back, tree_a = wait_for_text("Start B", 30)
want(back, "back-from-a-after-b-finishes", {"probe_seq": (tree_a or {}).get("probe_seq")})

# MPROC5c: the idle kill. B holds nothing now (its Activity is finished and its
# window went with it), so the launcher must take it within the timeout.
kill = None
deadline = time.time() + idle_secs + 45
while time.time() < deadline:
    table = ctl_processes()
    killed = table.get("killed") or []
    if killed:
        kill = (table, killed[-1])
        break
    time.sleep(0.5)
table, kill_record = kill if kill else ({}, {})
want(
    kill_record.get("reason") == "idle" and kill_record.get("tag") == 1,
    "idle-kill-recorded",
    kill_record,
)
want(
    kill_record.get("pid") == first_pid
    and int(kill_record.get("idle_ms") or 0) >= idle_secs * 1000,
    "idle-kill-after-the-timeout",
    {"pid": kill_record.get("pid"), "first_pid": first_pid, "idle_ms": kill_record.get("idle_ms")},
)
want(
    len(table.get("processes") or []) == 0,
    "process-record-removed",
    {"processes": table.get("processes")},
)
want(
    int(kill_record.get("window_sessions") or 0) >= 1,
    "killed-process-owned-a-window-session",
    {"window_sessions": kill_record.get("window_sessions")},
)

# The peer closed the dead process's session (and dropped its binders) when the
# guest's connection closed: the launcher's own stderr is the evidence.
closed = []
deadline = time.time() + 20
while time.time() < deadline:
    text = open(stderr_path, errors="replace").read()
    closed = re.findall(r"process tag 1 disconnected: window session (\d+) closed", text)
    if closed:
        break
    time.sleep(0.5)
want(bool(closed), "session-window-closed-on-death", {"sessions": closed})

# Next intent: the same button, a **new** process.
respawn_reply, _ = tap_text("Start B", tree_a)
want(
    respawn_reply is not None and respawn_reply.get("ok") is True,
    "tap-start-b-again",
    respawn_reply,
)
respawned = None
deadline = time.time() + 45
while time.time() < deadline:
    table2 = ctl_processes()
    live = [p for p in (table2.get("processes") or []) if p.get("tag") == 1]
    if live:
        respawned = (table2, live[0])
        break
    time.sleep(0.5)
table2, live = respawned if respawned else ({}, {})
want(
    live.get("pid") is not None and live.get("pid") != first_pid,
    "next-intent-respawns-the-process",
    {"pid": live.get("pid"), "first_pid": first_pid},
)
spawn_records = [
    r
    for r in ((table2 or {}).get("requests") or [])
    if r.get("package") == "dev.acos.fixtures.procb" and r.get("spawned") is True
]
want(
    len(spawn_records) >= 2,
    "spawn-recorded-twice",
    {"spawns": spawn_records},
)
on_top_again, tree_b2 = wait_for_text("Finish B", 30)
want(on_top_again, "respawned-window-on-top", {"probe_seq": (tree_b2 or {}).get("probe_seq")})

# A second `created` line can only come from a new process instance.
created = probe_lines("b created")
want(
    len(created) >= 2,
    "proc-b-logged-created-in-a-new-process",
    {"lines": created[:4]},
)

send({"cmd": "close"})
try:
    proc.wait(timeout=90)
except subprocess.TimeoutExpired:
    proc.kill()
    proc.wait()
stderr_file.close()
print("EXIT", proc.returncode)
print("FACTS", json.dumps(facts, indent=2)[:4000])
print("FAILURES", json.dumps(failures))
sys.exit(0 if not failures else 1)
