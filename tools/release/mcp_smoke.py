#!/usr/bin/env python3
"""Drive a *installed* `acos-mcp` over MCP stdio and check the agent loop.

This is the REL1c release smoke test: it talks to the server exactly as an MCP
client would (newline-delimited JSON-RPC 2.0 on stdin/stdout), from an empty
directory outside any checkout, and proves that the installed server drives a
real session and a real scenario:

    initialize -> tools/list -> acos_help
    -> acos_launch (APK) -> acos_observe_tree ("0", "Increment")
    -> acos_tap Increment -> acos_observe_tree ("1")
    -> acos_destroy
    -> acos_verify (optional scenario) -> verdict.json pass

Every request and response frame is printed as one JSON line (so a run can be
recorded verbatim), followed by one `{"check": ...}` line per assertion and a
final `{"smoke": "ok"|"fail", ...}` line. Exit status is 0 only when every check
passed: a non-pass envelope or a missing node is a failure, never a skip.

Usage:
  tools/release/mcp_smoke.py --mcp-bin <prefix>/bin/acos-mcp \\
      --apk /path/to/counter.apk \\
      [--scenario /path/to/gaten-home.yaml --scenario-apk /path/to/gaten.apk \\
       --artifacts /path/to/rel1c-artifacts] [--timeout 600]

The session engine (in-process Rust VM vs a running `acos serve --engine art`
daemon) is whatever the server's environment selects (`ACOS_SESSION_DAEMON`,
`ACOS_SESSION_SOCKET`), so the same client covers both.
"""

from __future__ import annotations

import argparse
import json
import os
import select
import subprocess
import sys
import time
from pathlib import Path

PROTOCOL_VERSION = "2025-03-26"


class SmokeError(Exception):
    pass


def emit(obj):
    sys.stdout.write(json.dumps(obj, sort_keys=True) + "\n")
    sys.stdout.flush()


class Mcp:
    """A minimal blocking MCP stdio client (no third-party packages)."""

    def __init__(self, bin_path: Path, env: dict[str, str]):
        self.next_id = 1
        self.buf = b""
        self.proc = subprocess.Popen(
            [str(bin_path)],
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=sys.stderr,
            env=env,
            bufsize=0,
        )

    def send(self, frame: dict):
        emit({"dir": "send", "frame": frame})
        data = (json.dumps(frame) + "\n").encode()
        assert self.proc.stdin is not None
        self.proc.stdin.write(data)
        self.proc.stdin.flush()

    def _read_one(self, deadline: float) -> dict:
        assert self.proc.stdout is not None
        while b"\n" not in self.buf:
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                raise SmokeError("timeout waiting for a server frame")
            ready, _, _ = select.select([self.proc.stdout], [], [], remaining)
            if not ready:
                raise SmokeError("timeout waiting for a server frame")
            chunk = os.read(self.proc.stdout.fileno(), 1 << 20)
            if not chunk:
                code = self.proc.poll()
                raise SmokeError(f"server closed stdout (exit {code})")
            self.buf += chunk
        line, self.buf = self.buf.split(b"\n", 1)
        line = line.strip()
        if not line:
            return self._read_one(deadline)
        try:
            frame = json.loads(line)
        except json.JSONDecodeError as e:
            raise SmokeError(f"server wrote a non-JSON line: {line[:200]!r} ({e})")
        emit({"dir": "recv", "frame": frame})
        return frame

    def request(self, method: str, params: dict | None = None, timeout: float = 120.0) -> dict:
        rid = self.next_id
        self.next_id += 1
        frame = {"jsonrpc": "2.0", "id": rid, "method": method}
        if params is not None:
            frame["params"] = params
        self.send(frame)
        deadline = time.monotonic() + timeout
        while True:
            reply = self._read_one(deadline)
            if reply.get("id") == rid:
                if "error" in reply:
                    raise SmokeError(f"{method} returned JSON-RPC error: {reply['error']}")
                return reply["result"]

    def notify(self, method: str, params: dict | None = None):
        frame = {"jsonrpc": "2.0", "method": method}
        if params is not None:
            frame["params"] = params
        self.send(frame)

    def call_tool(self, name: str, arguments: dict, timeout: float = 120.0) -> dict:
        return self.request(
            "tools/call", {"name": name, "arguments": arguments}, timeout=timeout
        )

    def close(self):
        try:
            if self.proc.stdin:
                self.proc.stdin.close()
        except OSError:
            pass
        try:
            self.proc.wait(timeout=30)
        except subprocess.TimeoutExpired:
            self.proc.kill()
            self.proc.wait()


def envelope(result: dict) -> dict:
    """The ACOS envelope carried as the tool result's first text item."""
    content = result.get("content") or []
    if not content or content[0].get("type") != "text":
        raise SmokeError(f"tool result has no text envelope: {result}")
    try:
        return json.loads(content[0]["text"])
    except json.JSONDecodeError as e:
        raise SmokeError(f"tool text is not JSON ({e}): {content[0]['text'][:200]!r}")


def check(name: str, ok: bool, detail=None):
    emit({"check": name, "ok": bool(ok), "detail": detail})
    if not ok:
        raise SmokeError(f"check failed: {name} ({detail})")


def node_texts(value, out: list[str]):
    if isinstance(value, dict):
        for key, item in value.items():
            if key == "text" and isinstance(item, str):
                out.append(item)
            else:
                node_texts(item, out)
    elif isinstance(value, list):
        for item in value:
            node_texts(item, out)


def tree_texts(env: dict) -> list[str]:
    out: list[str] = []
    node_texts(env.get("result", {}), out)
    return out


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--mcp-bin", required=True, help="the acos-mcp binary or wrapper")
    ap.add_argument("--apk", required=True, help="APK to launch (must be readable from here)")
    ap.add_argument("--scenario", help="optional scenario YAML to run with acos_verify")
    ap.add_argument("--scenario-apk", help="APK the scenario should use (app: rewritten)")
    ap.add_argument("--artifacts", help="artifacts dir for the verify call")
    ap.add_argument("--timeout", type=float, default=600.0, help="per tool call, seconds")
    args = ap.parse_args()

    mcp_bin = Path(args.mcp_bin).resolve()
    apk = Path(args.apk).resolve()
    for path, label in ((mcp_bin, "--mcp-bin"), (apk, "--apk")):
        if not path.exists():
            print(f"mcp_smoke: {label} {path} does not exist", file=sys.stderr)
            return 2

    env = dict(os.environ)
    emit(
        {
            "smoke": "start",
            "mcp_bin": str(mcp_bin),
            "apk": str(apk),
            "cwd": os.getcwd(),
            "acos_home": env.get("ACOS_HOME"),
            "acos_bin": env.get("ACOS_BIN"),
            "allowed_roots": env.get("ACOS_ALLOWED_ROOTS"),
            "session_daemon": env.get("ACOS_SESSION_DAEMON"),
        }
    )

    client = Mcp(mcp_bin, env)
    sid = None
    try:
        init = client.request(
            "initialize",
            {
                "protocolVersion": PROTOCOL_VERSION,
                "capabilities": {},
                "clientInfo": {"name": "rel1c-mcp-smoke", "version": "1"},
            },
        )
        check(
            "initialize",
            init.get("serverInfo", {}).get("name") == "acos-mcp"
            and init.get("protocolVersion") == PROTOCOL_VERSION,
            init.get("serverInfo"),
        )
        client.notify("notifications/initialized")

        tools = client.request("tools/list", {}).get("tools", [])
        names = {t.get("name") for t in tools}
        need = {
            "acos_help",
            "acos_launch",
            "acos_observe_tree",
            "acos_tap",
            "acos_destroy",
            "acos_verify",
            "acos_doctor",
        }
        check("tools/list", need <= names, sorted(names & need))
        available = {
            t["name"]: t.get("annotations", {}).get("available") for t in tools
        }
        check(
            "tools/available",
            available.get("acos_launch") is True
            and available.get("acos_verify") is True
            and available.get("acos_observe_tree") is True,
            available,
        )

        help_env = envelope(client.call_tool("acos_help", {}))
        guide = help_env.get("result", {}).get("guide") or ""
        check(
            "acos_help",
            help_env.get("status") == "pass" and len(guide) > 5000,
            {"chars": len(guide)},
        )

        launched = envelope(
            client.call_tool("acos_launch", {"apk": str(apk)}, timeout=args.timeout)
        )
        check(
            "acos_launch",
            launched.get("status") == "pass",
            {
                "status": launched.get("status"),
                "stage": launched.get("result", {}).get("reached_stage"),
                "engine": launched.get("result", {}).get("engine"),
                "error": launched.get("error"),
            },
        )
        sid = launched.get("session_id") or launched.get("result", {}).get("session_id")
        check("session_id", bool(sid), sid)

        tree0 = envelope(
            client.call_tool(
                "acos_observe_tree", {"session_id": sid}, timeout=args.timeout
            )
        )
        texts0 = tree_texts(tree0)
        check(
            "observe_tree initial",
            tree0.get("status") == "pass" and "0" in texts0 and "Increment" in texts0,
            {"texts": texts0},
        )

        tapped = envelope(
            client.call_tool(
                "acos_tap",
                {"session_id": sid, "text": "Increment"},
                timeout=args.timeout,
            )
        )
        check(
            "acos_tap Increment",
            tapped.get("status") == "pass"
            and tapped.get("result", {}).get("accepted") is True,
            {
                "status": tapped.get("status"),
                "accepted": tapped.get("result", {}).get("accepted"),
                "state_changed": tapped.get("result", {}).get("state_changed"),
                "error": tapped.get("error"),
            },
        )

        tree1 = envelope(
            client.call_tool(
                "acos_observe_tree", {"session_id": sid}, timeout=args.timeout
            )
        )
        texts1 = tree_texts(tree1)
        check(
            "observe_tree after tap",
            tree1.get("status") == "pass" and "1" in texts1 and "0" not in texts1,
            {"texts": texts1},
        )

        destroyed = envelope(
            client.call_tool("acos_destroy", {"session_id": sid}, timeout=args.timeout)
        )
        check(
            "acos_destroy",
            destroyed.get("status") == "pass"
            and destroyed.get("result", {}).get("destroyed") is True,
            {"status": destroyed.get("status"), "result": destroyed.get("result")},
        )
        sid = None

        if args.scenario:
            scenario = Path(args.scenario).resolve()
            artifacts = Path(args.artifacts or (Path.cwd() / "acos-artifacts")).resolve()
            if not scenario.is_file():
                raise SmokeError(f"--scenario {scenario} is not a file")
            artifacts.mkdir(parents=True, exist_ok=True)
            run_scenario = artifacts / (scenario.stem + ".scenario.yaml")
            text = scenario.read_text()
            scenario_apk = (
                Path(args.scenario_apk).resolve()
                if args.scenario_apk
                else apk
            )
            if not scenario_apk.is_file():
                raise SmokeError(f"--scenario-apk {scenario_apk} is not a file")
            # Rewrite only the `app:` line; every step and setting stays as
            # authored, so a pass still means what the original scenario means.
            lines = []
            replaced = False
            for line in text.splitlines():
                if line.startswith("app:"):
                    lines.append(f"app: {scenario_apk}")
                    replaced = True
                else:
                    lines.append(line)
            if not replaced:
                raise SmokeError(f"{scenario} has no top-level `app:` line")
            run_scenario.write_text("\n".join(lines) + "\n")
            emit(
                {
                    "scenario": str(run_scenario),
                    "scenario_app": str(scenario_apk),
                    "artifacts": str(artifacts),
                }
            )
            verified = envelope(
                client.call_tool(
                    "acos_verify",
                    {
                        "scenario": str(run_scenario),
                        "artifacts": str(artifacts),
                        "apk": str(scenario_apk),
                        "engine": "art",
                        "base": {"request_id": "rel1c_verify"},
                    },
                    timeout=args.timeout,
                )
            )
            verdict_path = artifacts / "verdict.json"
            verdict = (
                json.loads(verdict_path.read_text()) if verdict_path.is_file() else None
            )
            verdict_state = (
                None if verdict is None else (verdict.get("verdict") or verdict.get("status"))
            )
            check(
                "acos_verify",
                verified.get("status") == "pass" and verdict_state == "pass",
                {
                    "status": verified.get("status"),
                    "verdict": verdict_state,
                    "steps": None if verdict is None else len(verdict.get("steps", [])),
                    "error": verified.get("error"),
                },
            )

        emit({"smoke": "ok"})
        return 0
    except SmokeError as e:
        emit({"smoke": "fail", "error": str(e)})
        return 1
    finally:
        try:
            if sid:
                # Best effort: never leave a guest behind on a failed run.
                client.call_tool("acos_destroy", {"session_id": sid}, timeout=120)
        except Exception as e:  # noqa: BLE001 - teardown must not mask the result
            print(f"mcp_smoke: destroy during teardown failed: {e}", file=sys.stderr)
        client.close()


if __name__ == "__main__":
    sys.exit(main())
