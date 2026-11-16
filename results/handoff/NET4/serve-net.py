#!/usr/bin/env python3
"""NET3: the `acos_net` serve verb, driven twice.

1. **Through a live `acos serve` daemon** (the real product path: one daemon, one
   real ART session, the daemon's own newline-JSON socket protocol described in
   `acos-mcp/src/daemon.rs`). Every request and reply is recorded verbatim.
2. **Through MCP** (`acos-mcp`, JSON-RPC over stdio) with
   `ACOS_SESSION_DAEMON=1` pointing at that daemon: `initialize`,
   `tools/list` (is `acos_net` advertised?), then a `tools/call` for it.

Nothing here is synthesised: a reply is printed as it came off the wire, and the
checks are assertions on the parsed replies.

Usage (inside the ART lock + guard):
  ACOS_SESSION_SOCKET=…/session.sock python3 results/handoff/NET3/serve-net.py OUTDIR
"""
from __future__ import annotations

import json
import os
import socket
import subprocess
import sys
import time
from pathlib import Path

HERE = Path(__file__).resolve().parent
WT = HERE.parents[2]
BIN = Path(os.environ.get("ACOS", WT / ".cache/net4-target/release/acos"))
MCP = Path(os.environ.get("ACOS_MCP", WT / ".cache/net4-target/release/acos-mcp"))
OUT = Path(sys.argv[1] if len(sys.argv) > 1 else HERE / "run" / "serve")
OUT.mkdir(parents=True, exist_ok=True)
SOCK = Path(os.environ.get("ACOS_SESSION_SOCKET", WT / ".cache/net4/session.sock"))
APK = WT / "fixtures/build/apps/net-check.apk"
SID = "net4-serve"

transcript = (OUT / "serve-net.log").open("w")


def say(line: str) -> None:
    print(line)
    transcript.write(line + "\n")
    transcript.flush()


def rpc(sock: socket.socket, req_id: int, tool: str, args: dict, fh) -> dict:
    line = json.dumps({"id": req_id, "tool": tool, "args": args})
    say(f">>> {line}")
    sock.sendall(line.encode() + b"\n")
    buf = b""
    while not buf.endswith(b"\n"):
        chunk = sock.recv(65536)
        if not chunk:
            raise SystemExit("daemon closed the connection")
        buf += chunk
    text = buf.decode()
    say(f"<<< {text.rstrip()}")
    return json.loads(text)


def envelope(reply: dict) -> dict:
    return reply.get("envelope") or {}


def main() -> int:
    for p in (SOCK, Path(str(SOCK) + ".lock")):
        try:
            p.unlink()
        except FileNotFoundError:
            pass
    SOCK.parent.mkdir(parents=True, exist_ok=True)
    env = dict(os.environ)
    env["ACOS_SESSION_SOCKET"] = str(SOCK)
    env.pop("ACOS_SESSION_DAEMON", None)
    daemon = subprocess.Popen(
        [str(BIN), "serve", "--engine", "art", "--socket", str(SOCK), "--json",
         "--idle-timeout", "120", "--owner-pid", str(os.getpid())],
        env=env, stdout=subprocess.PIPE, stderr=(OUT / "daemon.err").open("w"),
        text=True)
    banner = daemon.stdout.readline().strip()
    say(f"daemon: {banner}")
    for _ in range(200):
        if SOCK.exists():
            break
        time.sleep(0.05)

    checks: list[tuple[str, bool, str]] = []
    s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    s.settimeout(300)
    s.connect(str(SOCK))
    f = OUT / "daemon.frames"
    fh = f.open("w")

    launch = rpc(s, 1, "acos_launch",
                 {"base": {"request_id": "r-launch", "session_id": SID},
                  "apk": str(APK), "engine": "art"}, fh)
    ok = envelope(launch).get("ok") is True
    checks.append(("daemon_launch", ok, str(envelope(launch).get("error"))))

    saw_offline = False
    saw_delay = False
    saw_hostport = False
    for i, (action, extra) in enumerate([
        ("status", {}),
        ("offline", {}),
        ("status", {}),
        ("online", {}),
        ("delay", {"ms": 400, "loss_pct": 1}),
        ("delay", {"ms": 0, "loss_pct": 0}),
        ("hostport", {"port": 18081, "on": True}),
        ("hostport", {"port": 18081, "on": False}),
        ("status", {}),
    ], start=2):
        args = {"base": {"request_id": f"r-{i}", "session_id": SID}, "action": action, **extra}
        r = rpc(s, i, "acos_net", args, fh)
        env_r = envelope(r)
        if action == "status" and i > 2:
            row = (env_r.get("result") or {}).get("session") or {}
            saw_offline = saw_offline or bool(row.get("offline"))
        if action == "delay" and extra.get("ms"):
            row = (env_r.get("result") or {}).get("session") or {}
            saw_delay = True if row.get("delay_ms") == 400 else saw_delay
        if action == "hostport" and extra.get("on"):
            res = env_r.get("result") or {}
            saw_hostport = 18081 in (res.get("host_ports") or []) or saw_hostport
        if env_r.get("ok") is not True:
            checks.append((f"daemon_{action}", False, str(env_r.get("error"))))
    checks.append(("daemon_status_reports_offline", saw_offline, "session row offline=true after `offline`"))
    checks.append(("daemon_delay_reported", saw_delay, "session row delay_ms=400"))
    checks.append(("daemon_hostport_reported", saw_hostport, "host_ports contains 18081"))

    # NET4: the session is destroyed *after* the MCP section — it is the session
    # the MCP call addresses, and an early destroy made the real `acos_net` call
    # answer SESSION_NOT_FOUND (a correct answer to the wrong question).

    # --- MCP ---------------------------------------------------------------
    if not MCP.exists():
        checks.append(("mcp_binary", False, f"{MCP} not built"))
    else:
        mcp_env = dict(env)
        mcp_env["ACOS_SESSION_DAEMON"] = "1"
        mcp = subprocess.Popen([str(MCP)], env=mcp_env, stdin=subprocess.PIPE,
                               stdout=subprocess.PIPE, stderr=(OUT / "mcp.err").open("w"),
                               text=True)

        def send(obj):
            raw = json.dumps(obj)
            say(f">>> {raw}")
            mcp.stdin.write(raw + "\n")
            mcp.stdin.flush()
            line = mcp.stdout.readline().strip()
            say(f"<<< {line}")
            return json.loads(line)

        send({"jsonrpc": "2.0", "id": 1, "method": "initialize",
              "params": {"protocolVersion": "2025-03-26", "capabilities": {},
                         "clientInfo": {"name": "net4-serve", "version": "1"}}})
        listed = send({"jsonrpc": "2.0", "id": 2, "method": "tools/list", "params": {}})
        names = [t.get("name") for t in listed.get("result", {}).get("tools", [])]
        checks.append(("mcp_lists_acoustic_tools", bool(names), f"{len(names)} tools"))
        advertised = "acos_net" in names
        # NET4: the MCP descriptor must be the session API's own verb: same name,
        # same closed argument schema.
        schema = next((t.get("inputSchema") for t in listed.get("result", {}).get("tools", [])
                       if t.get("name") == "acos_net"), None)
        checks.append(("mcp_acos_net_advertised", advertised,
                       f"tools/list names acos_net: {advertised}"))
        checks.append(("mcp_acos_net_schema_closed",
                       bool(schema) and schema.get("additionalProperties") is False,
                       json.dumps(schema.get("properties", {}).keys() and
                                  sorted(schema.get("properties", {})) or schema)[:200]))
        call = send({"jsonrpc": "2.0", "id": 3, "method": "tools/call",
                     "params": {"name": "acos_net",
                                "arguments": {"base": {"request_id": "r-mcp", "session_id": SID},
                                              "action": "status"}}})
        err = (call.get("error") or {}).get("message", "")
        result = (call.get("result") or {})
        text = json.dumps(result)
        # The MCP reply is a *bound* envelope whose text is a JSON string, so the
        # fields are read out of the parsed daemon reply rather than grepped out
        # of the rendered content.
        inner = {}
        for block in result.get("content", []):
            try:
                inner = json.loads(block.get("text", "{}"))
            except ValueError:
                continue
        daemon_result = inner.get("result") or {}
        checks.append((
            "mcp_acos_net_status",
            not err and inner.get("ok") is True
            and daemon_result.get("action") == "status"
            and daemon_result.get("networked") is True,
            f"ok={inner.get('ok')} action={daemon_result.get('action')} "
            f"networked={daemon_result.get('networked')} relay_up={(daemon_result.get('relay') or {}).get('up')}",
        ))
        # A closed schema over MCP too: an unknown argument is a structured
        # INVALID_ARGUMENT from the daemon, never a silent success.
        bad = send({"jsonrpc": "2.0", "id": 4, "method": "tools/call",
                    "params": {"name": "acos_net",
                               "arguments": {"base": {"request_id": "r-mcp-bad", "session_id": SID},
                                             "offline": True}}})
        badtext = json.dumps(bad)
        checks.append(("mcp_acos_net_rejects_unknown_argument",
                       "unknown argument" in badtext and "offline" in badtext,
                       badtext[:200]))
        try:
            mcp.stdin.close()
        except Exception:
            pass
        mcp.wait(timeout=30)

    # Destroy the session on the same daemon, then stop it.
    s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    s.connect(str(SOCK))
    fh = (OUT / "serve-net-destroy.log").open("w")
    rpc(s, 21, "acos_destroy", {"base": {"request_id": "r-destroy", "session_id": SID}}, fh)
    s.close()
    fh.close()
    daemon.terminate()
    daemon.wait(timeout=30)
    failed = [c for c in checks if not c[1]]
    for name, ok, detail in checks:
        say(f"check={name} {'PASS' if ok else 'FAIL'} {detail}")
    say(f"== {len(checks) - len(failed)} PASS / {len(failed)} FAIL")
    return 0 if not failed else 1


if __name__ == "__main__":
    sys.exit(main())
