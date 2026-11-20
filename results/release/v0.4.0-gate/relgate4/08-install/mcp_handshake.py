#!/usr/bin/env python3
"""RELGATE3 item 5: minimal MCP handshake against the installed wrapper.

Sends `initialize` then `tools/list` over stdio and reports the server identity,
the protocol version and the advertised tool count/names. No ART session is
started (the brief asks for initialize + tools/list only).
"""
import json
import os
import subprocess
import sys

mcp_bin = sys.argv[1]
out_path = sys.argv[2]

env = {k: v for k, v in os.environ.items()
       if k not in ("ACOS_HOME", "ACOS_BOOT_DIR", "XDG_RUNTIME_DIR")}

proc = subprocess.Popen([mcp_bin], stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                        stderr=subprocess.PIPE, text=True, env=env, bufsize=1)


def send(obj):
    proc.stdin.write(json.dumps(obj) + "\n")
    proc.stdin.flush()


def recv():
    line = proc.stdout.readline()
    if not line:
        raise SystemExit("acos-mcp closed stdout")
    return json.loads(line)


rows = []
send({"jsonrpc": "2.0", "id": 1, "method": "initialize",
      "params": {"protocolVersion": "2024-11-05",
                 "capabilities": {},
                 "clientInfo": {"name": "relgate3-smoke", "version": "1"}}})
init = recv()
rows.append({"step": "initialize", "reply": init})

send({"jsonrpc": "2.0", "method": "notifications/initialized", "params": {}})
send({"jsonrpc": "2.0", "id": 2, "method": "tools/list", "params": {}})
tools = recv()
names = [t["name"] for t in tools.get("result", {}).get("tools", [])]
rows.append({"step": "tools/list", "count": len(names), "tools": names,
             "reply_ok": "result" in tools})

proc.stdin.close()
try:
    proc.wait(timeout=20)
except subprocess.TimeoutExpired:
    proc.kill()
err = proc.stderr.read()

with open(out_path, "w") as f:
    for r in rows:
        f.write(json.dumps(r) + "\n")

print("server", json.dumps(init.get("result", {}).get("serverInfo", {})))
print("protocolVersion", init.get("result", {}).get("protocolVersion"))
print("tools", len(names), names)
print("stderr_tail", err.strip().splitlines()[-3:] if err.strip() else [])
