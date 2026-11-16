#!/usr/bin/env python3
"""MOD1 evidence: acos_services on a live ART session over MCP (stdio)."""
import json, os, subprocess, sys, time

BIN = os.environ["ACOS_BIN"]
APK = os.environ["MOD1_APK"]
MCP = os.path.join(os.path.dirname(BIN), "acos-mcp")
proc = subprocess.Popen(
    [MCP],
    stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True,
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

out = {}
rpc("initialize", {"protocolVersion": "2025-03-26", "capabilities": {}, "clientInfo": {"name": "mod1", "version": "1"}})
rpc("notifications/initialized", {}, notify=True)

def tool(name, args):
    answer = rpc("tools/call", {"name": name, "arguments": args})
    return answer["result"] if "result" in answer else answer

def envelope(result):
    if isinstance(result, dict) and "content" in result:
        return json.loads(result["content"][0]["text"])
    return result

sid = "s_mod1_live"
launch = envelope(tool("acos_launch", {"apk": APK, "engine": "art", "base": {"session_id": sid},
                                      "services": [{"id": "feedback.vibrator", "profile": "stub"}]}))
out["launch"] = {"status": launch.get("status"), "modules": launch.get("result", {}).get("modules")}
out["list"] = envelope(tool("acos_services", {"op": "list", "base": {"session_id": sid}}))
out["detach_core"] = envelope(tool("acos_services", {"op": "detach", "id": "core", "base": {"session_id": sid}}))
out["detach_hot"] = envelope(tool("acos_services", {"op": "detach", "id": "feedback.vibrator", "base": {"session_id": sid}}))
out["attach_hot"] = envelope(tool("acos_services", {"op": "attach", "id": "feedback.vibrator", "profile": "stub", "base": {"session_id": sid}}))
caps = envelope(tool("acos_capabilities", {"session_id": sid}))
out["capabilities_session"] = caps.get("result", {}).get("session")
out["destroy"] = envelope(tool("acos_destroy", {"base": {"session_id": sid}})).get("status")
proc.stdin.close()
proc.wait(timeout=120)
print(json.dumps(out, indent=2))
