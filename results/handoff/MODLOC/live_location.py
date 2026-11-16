#!/usr/bin/env python3
"""MODLOC evidence: the location module on a **live** ART session over MCP.

Proves what a scenario cannot express today (there is no scenario step for a live
module detach, see results/handoff/MODLOC/README.md):

1. `acos_services {op: ctl}` feeds a fix that the app shows on screen;
2. after `acos_services {op: detach, id: location}` the app's next location call
   takes the honest disabled path (a structured error, `last=none`, `fix=none`),
   never a stale fix and never a silent success.

Run:  ACOS_BIN=... MODLOC_APK=... python3 results/handoff/MODLOC/live_location.py
"""
import json
import os
import subprocess

BIN = os.environ["ACOS_BIN"]
APK = os.environ["MODLOC_APK"]
MCP = os.path.join(os.path.dirname(BIN), "acos-mcp")
proc = subprocess.Popen(
    [MCP],
    stdin=subprocess.PIPE,
    stdout=subprocess.PIPE,
    stderr=subprocess.PIPE,
    text=True,
    env={**os.environ, "ACOS_BIN": BIN},
)
_next = [0]


def rpc(method, params, notify=False):
    if not notify:
        _next[0] += 1
    msg = {"jsonrpc": "2.0", "method": method, "params": params}
    if not notify:
        msg["id"] = _next[0]
    proc.stdin.write(json.dumps(msg) + "\n")
    proc.stdin.flush()
    if notify:
        return None
    while True:
        line = proc.stdout.readline()
        if not line:
            raise RuntimeError("server closed: " + proc.stderr.read())
        answer = json.loads(line)
        if answer.get("id") == _next[0]:
            return answer


rpc(
    "initialize",
    {
        "protocolVersion": "2025-03-26",
        "capabilities": {},
        "clientInfo": {"name": "modloc", "version": "1"},
    },
)
rpc("notifications/initialized", {}, notify=True)


def tool(name, args):
    answer = rpc("tools/call", {"name": name, "arguments": args})
    return answer["result"] if "result" in answer else answer


def envelope(result):
    if isinstance(result, dict) and "content" in result:
        return json.loads(result["content"][0]["text"])
    return result


def screen_texts(session):
    """Bare node id -> text for the session's current tree."""
    reply = envelope(tool("acos_observe_tree", {"base": {"session_id": session}}))
    found = {}

    def walk(node):
        if not isinstance(node, dict):
            return
        rid = node.get("resource_id")
        text = node.get("text")
        if rid and text:
            found[str(rid).split("/")[-1]] = text
        for child in node.get("children") or []:
            walk(child)

    roots = (((reply or {}).get("result") or {}).get("tree") or {}).get("nodes") or []
    for root in roots:
        walk(root)
    return found


def screen_of(session):
    found = screen_texts(session)
    return {key: found.get(key) for key in ("state", "last", "fix")}


out = {}
sid = "s_modloc_live"
launch = envelope(
    tool(
        "acos_launch",
        {
            "apk": APK,
            "engine": "art",
            "base": {"session_id": sid},
            "services": [
                {
                    "id": "location",
                    "profile": "scripted",
                    "config": {"permissions": "fine"},
                }
            ],
        },
    )
)
out["launch"] = {
    "status": launch.get("status"),
    "modules": (launch.get("result") or {}).get("modules"),
}
out["after_launch"] = screen_of(sid)

ctl = envelope(
    tool(
        "acos_services",
        {
            "op": "ctl",
            "id": "location",
            "ctl_op": "set_location",
            "args": {
                "lat": 37.4220,
                "lng": -122.0841,
                "accuracy": 4.0,
                "provider": "gps",
            },
            "base": {"session_id": sid},
        },
    )
)
out["ctl_set_location"] = {
    "status": ctl.get("status"),
    "result": (ctl.get("result") or {}).get("result"),
}
out["tap_recheck"] = {
    "status": envelope(tool("acos_tap", {"id": "check", "base": {"session_id": sid}})).get(
        "status"
    )
}
out["after_set_location"] = screen_of(sid)

detach = envelope(
    tool("acos_services", {"op": "detach", "id": "location", "base": {"session_id": sid}})
)
out["detach"] = {
    "status": detach.get("status"),
    "modules": [
        row
        for row in (detach.get("result") or {}).get("modules") or []
        if row.get("id") == "location"
    ],
}
out["tap_recheck_after_detach"] = {
    "status": envelope(tool("acos_tap", {"id": "check", "base": {"session_id": sid}})).get(
        "status"
    )
}
out["after_detach"] = screen_of(sid)
out["destroy"] = envelope(tool("acos_destroy", {"base": {"session_id": sid}})).get("status")

checks = {
    "launch_shows_ready": (out["after_launch"].get("state") or "").startswith("state=ready"),
    "fix_reaches_the_app": "37.4220" in (out["after_set_location"].get("last") or ""),
    "detach_leaves_no_fix": (out["after_detach"].get("fix") or "") == "fix=none",
    "detach_shows_the_disabled_path": (out["after_detach"].get("state") or "").startswith(
        "state=unavailable"
    ),
    "detach_reports_off": (out["detach"].get("modules") or [{}])[0].get("state") == "off",
}
out["checks"] = checks
out["pass"] = all(checks.values())
proc.stdin.close()
proc.wait(timeout=120)
print(json.dumps(out, indent=2))
raise SystemExit(0 if out["pass"] else 1)
