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


def clickable_texts(env: dict) -> list[str]:
    """Texts of nodes the user can actually tap (visible and clickable)."""
    nodes: list[dict] = []

    def walk(value):
        if isinstance(value, dict):
            if "clickable" in value and "text" in value:
                nodes.append(value)
            for item in value.values():
                walk(item)
        elif isinstance(value, list):
            for item in value:
                walk(item)

    walk(env.get("result", {}))
    seen: list[str] = []
    for node in nodes:
        text = (node.get("text") or "").strip()
        if (
            text
            and node.get("clickable") is True
            and node.get("visible") is True
            and text not in seen
        ):
            seen.append(text)
    return seen


def node_center(env: dict, text: str):
    """Centre of the first tree node whose text matches, from its bounds."""
    found = []

    def walk(value):
        if isinstance(value, dict):
            if value.get("text") == text and value.get("bounds"):
                found.append(value)
            for item in value.values():
                walk(item)
        elif isinstance(value, list):
            for item in value:
                walk(item)

    walk(env.get("result", {}))
    for node in found:
        raw = node.get("visible_bounds") or node.get("bounds")
        try:
            left, rest = raw.split("][")
            x1, y1 = left.lstrip("[").split(",")
            x2, y2 = rest.rstrip("]").split(",")
            x = (int(x1) + int(x2)) // 2
            y = (int(y1) + int(y2)) // 2
            if int(x2) > int(x1) and int(y2) > int(y1):
                return x, y
        except (AttributeError, ValueError):
            continue
    return None


def png_ihdr(data: bytes):
    """(width, height) from a PNG's IHDR, or None when the bytes are not a PNG."""
    if len(data) < 33 or data[:8] != b"\x89PNG\r\n\x1a\n":
        return None
    if data[12:16] != b"IHDR":
        return None
    width = int.from_bytes(data[16:20], "big")
    height = int.from_bytes(data[20:24], "big")
    return width, height


def check_screenshot(env: dict, result: dict, args):
    """FB2b: `acos_observe_tree {"screenshot": true}` gives a real, bounded PNG."""
    import base64

    shot = (env.get("result") or {}).get("screenshot")
    check(
        "observe_tree_screenshot_status",
        env.get("status") == "pass" and isinstance(shot, dict),
        {"status": env.get("status"), "screenshot": shot, "error": env.get("error")},
    )
    raw_path = shot.get("path") or ""
    path = Path(raw_path)
    check(
        "screenshot_path_absolute_and_exists",
        bool(raw_path) and path.is_absolute() and path.is_file(),
        {"path": raw_path, "exists": path.is_file()},
    )
    size = path.stat().st_size
    check(
        "screenshot_bytes_under_1MB",
        0 < size < 1024 * 1024,
        {"bytes": size, "reported": shot.get("bytes")},
    )
    data = path.read_bytes()
    ihdr = png_ihdr(data)
    check(
        "screenshot_is_a_png",
        ihdr is not None and shot.get("format") == "png",
        {"format": shot.get("format"), "ihdr": ihdr, "magic": data[:8].hex()},
    )
    display_size = shot.get("display_size") or []
    origin = shot.get("display_origin")
    width, height = ihdr
    check(
        "screenshot_geometry",
        display_size == [1080, 2400]
        and isinstance(origin, list)
        and len(origin) == 2
        and all(isinstance(v, int) for v in origin)
        and width == shot.get("width")
        and height == shot.get("height")
        and 0 < width <= 2400
        and 0 < height <= 2400,
        {
            "ihdr": ihdr,
            "reported": [shot.get("width"), shot.get("height")],
            "display_size": display_size,
            "display_origin": origin,
        },
    )

    content = result.get("content") or []
    images = [b for b in content if b.get("type") == "image"]
    links = [b for b in content if b.get("type") == "resource_link"]
    # The envelope itself is the first text block; the preview's own line is the
    # one that names the lossless file.
    notes = [
        b.get("text", "")
        for b in content
        if b.get("type") == "text" and "lossless file:" in b.get("text", "")
    ]
    note = notes[0] if notes else ""
    check(
        "screenshot_inline_preview_present",
        len(images) == 1 and images[0].get("mimeType") == "image/png",
        {"images": len(images), "mime": [i.get("mimeType") for i in images]},
    )
    preview_bytes = base64.b64decode(images[0]["data"])
    max_bytes = int(os.environ.get("ACOS_MCP_MAX_IMAGE_BYTES", 512 * 1024))
    max_side = int(os.environ.get("ACOS_MCP_PREVIEW_MAX_SIDE", 1280))
    preview_ihdr = png_ihdr(preview_bytes)
    check(
        "screenshot_inline_preview_bounded",
        preview_ihdr is not None
        and len(preview_bytes) <= max_bytes
        and max(preview_ihdr) <= max_side,
        {
            "bytes": len(preview_bytes),
            "max_bytes": max_bytes,
            "ihdr": preview_ihdr,
            "max_side": max_side,
        },
    )
    check(
        "screenshot_preview_names_the_lossless_file",
        bool(note)
        and raw_path in note
        and (not links or any(raw_path in link.get("uri", "") for link in links)),
        {"note": note, "links": links},
    )
    emit(
        {
            "screenshot_evidence": {
                "path": raw_path,
                "bytes": size,
                "ihdr": ihdr,
                "display_size": display_size,
                "display_origin": origin,
                "preview_bytes": len(preview_bytes),
                "preview_ihdr": preview_ihdr,
            }
        }
    )


def destroy_and_check(client: "Mcp", sid: str, args):
    destroyed = envelope(
        client.call_tool("acos_destroy", {"session_id": sid}, timeout=args.timeout)
    )
    check(
        "acos_destroy",
        destroyed.get("status") == "pass"
        and destroyed.get("result", {}).get("destroyed") is True,
        {"status": destroyed.get("status"), "result": destroyed.get("result")},
    )
    gone = envelope(
        client.call_tool("acos_status", {"session_id": sid}, timeout=args.timeout)
    )
    check(
        "status_after_destroy",
        gone.get("status") != "pass"
        and (gone.get("error") or {}).get("code")
        in {"SESSION_NOT_FOUND", "UNSUPPORTED_BACKEND"},
        {
            "status": gone.get("status"),
            "code": (gone.get("error") or {}).get("code"),
        },
    )


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--mcp-bin", required=True, help="the acos-mcp binary or wrapper")
    ap.add_argument("--apk", required=True, help="APK to launch (must be readable from here)")
    ap.add_argument("--scenario", help="optional scenario YAML to run with acos_verify")
    ap.add_argument("--scenario-apk", help="APK the scenario should use (app: rewritten)")
    ap.add_argument("--artifacts", help="artifacts dir for the verify call")
    ap.add_argument("--timeout", type=float, default=600.0, help="per tool call, seconds")
    ap.add_argument(
        "--mode",
        choices=["counter", "gaten", "gaten-shot"],
        default="counter",
        help="counter: 0 -> tap Increment -> 1 -> reset -> 0; gaten: launch, read the "
        "real RN tree, tap a bottom tab and require the tree to change; gaten-shot: "
        "launch and ask acos_observe_tree for {screenshot: true}, then check the PNG "
        "on disk (exists, < 1 MB, decodes, IHDR size, display_origin) and that the "
        "reply carries a bounded inline preview",
    )
    ap.add_argument(
        "--tap-text",
        help="counter mode: node text to tap (default Increment); gaten mode: the "
        "bottom-tab label to tap (default: the first candidate present in the tree)",
    )
    ap.add_argument(
        "--verify-engine",
        help="optional engine argument for acos_verify. Omitted means the tool's "
        "default (real ART); pass rust-vm only for the frozen VM",
    )
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

        # acos_help with no topic returns the short overview (REL3b); the whole
        # guide is topic "all".
        help_env = envelope(client.call_tool("acos_help", {}))
        overview = help_env.get("result", {}).get("guide") or ""
        full_env = envelope(client.call_tool("acos_help", {"topic": "all"}))
        guide = full_env.get("result", {}).get("guide") or ""
        check(
            "acos_help",
            help_env.get("status") == "pass"
            and 1000 < len(overview) < 6000
            and full_env.get("status") == "pass"
            and len(guide) > 5000,
            {"overview_chars": len(overview), "full_chars": len(guide)},
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

        # The default engine is real ART. A default run must say so, both in the
        # envelope fidelity and in the launch result, with an honest ladder that
        # ends at first_frame.
        result0 = launched.get("result") or {}
        stages = result0.get("stages") or []
        ladder_ok = bool(stages) and all(
            stage.get("ok") is True for stage in stages
        ) and any(stage.get("stage") == "first_frame" for stage in stages)
        check(
            "engine_art_reference",
            launched.get("fidelity") == "art-reference"
            and result0.get("engine") == "art"
            and result0.get("stage_reached") == "first_frame"
            and ladder_ok,
            {
                "fidelity": launched.get("fidelity"),
                "engine": result0.get("engine"),
                "stage_reached": result0.get("stage_reached"),
                "stages": stages,
            },
        )

        observe_args: dict = {"session_id": sid}
        if args.mode == "gaten-shot":
            # The FB2 item: a screenshot through the observe verb an agent
            # already calls, not a separate capture tool.
            observe_args["screenshot"] = True
        tree0_result = client.call_tool(
            "acos_observe_tree", observe_args, timeout=args.timeout
        )
        tree0 = envelope(tree0_result)
        texts0 = tree_texts(tree0)

        if args.mode == "gaten-shot":
            check_screenshot(tree0, tree0_result, args)

        if args.mode == "gaten-shot":
            # Nothing to tap: the PNG + inline preview is the whole assertion.
            tap_text = None
        elif args.mode == "counter":
            check(
                "observe_tree initial",
                tree0.get("status") == "pass"
                and "0" in texts0
                and "Increment" in texts0,
                {"texts": texts0},
            )
            tap_text = args.tap_text or "Increment"
        else:
            real = [t for t in texts0 if t and t.lower() not in {"gaten"}]
            check(
                "observe_tree gaten",
                tree0.get("status") == "pass" and len(real) >= 5,
                {"texts": texts0, "non_empty": len(real)},
            )
            # A fresh Gaten install boots into onboarding, so the bottom tabs
            # may not exist yet; pick a node the tree says is visible and
            # clickable (a bottom tab when present, else the onboarding action).
            # React Native's tree reports `clickable`/`visible` as false on this
            # channel, so a text tap is refused as off-screen even for a node
            # the user sees. Read the label from the tree and tap its bounds.
            tabs = ["Home", "Recognition", "Lessons", "Kanji", "Search", "Settings"]
            ctas = ["Skip", "Open Gaten", "Continue", "Turn on lookup mode", "Set up Gaten"]
            present = [c for c in tabs + ctas if c in texts0]
            tap_text = args.tap_text or (present[0] if present else None)
            check(
                "gaten_tab_or_cta_present",
                tap_text is not None,
                {"present": present, "texts": texts0[:40]},
            )

        if args.mode == "gaten-shot":
            destroy_and_check(client, sid, args)
            emit({"smoke": "ok"})
            return 0
        if tap_text is None:
            raise SmokeError("no node text to tap")
        if args.mode == "gaten":
            center = node_center(tree0, tap_text)
            check(
                "gaten_tap_target_bounds",
                center is not None,
                {"text": tap_text, "center": center},
            )
            tap_args = {"session_id": sid, "x": center[0], "y": center[1]}
        else:
            tap_args = {"session_id": sid, "text": tap_text}
        tapped = envelope(
            client.call_tool("acos_tap", tap_args, timeout=args.timeout)
        )
        check(
            "acos_tap",
            tapped.get("status") == "pass"
            and tapped.get("result", {}).get("accepted") is True,
            {
                "text": tap_text,
                "args": tap_args,
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
        if args.mode == "counter":
            check(
                "observe_tree after tap",
                tree1.get("status") == "pass" and "1" in texts1 and "0" not in texts1,
                {"texts": texts1},
            )
        else:
            check(
                "gaten_tree_changed_after_tab",
                tree1.get("status") == "pass" and texts1 != texts0,
                {"before": texts0, "after": texts1},
            )

        if args.mode == "counter":
            reset = envelope(
                client.call_tool(
                    "acos_reset",
                    {"session_id": sid, "scope": "app"},
                    timeout=args.timeout,
                )
            )
            reset_result = reset.get("result") or {}
            check(
                "acos_reset",
                reset.get("status") == "pass"
                and reset_result.get("data_cleared") is True,
                {"status": reset.get("status"), "result": reset_result},
            )
            tree2 = envelope(
                client.call_tool(
                    "acos_observe_tree", {"session_id": sid}, timeout=args.timeout
                )
            )
            texts2 = tree_texts(tree2)
            check(
                "observe_tree after reset",
                tree2.get("status") == "pass" and "0" in texts2,
                {"texts": texts2},
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
        # After destroy the id is gone: status must say so, never `live: true`.
        gone = envelope(
            client.call_tool("acos_status", {"session_id": sid}, timeout=args.timeout)
        )
        check(
            "status_after_destroy",
            gone.get("status") != "pass"
            and (gone.get("error") or {}).get("code")
            in {"SESSION_NOT_FOUND", "UNSUPPORTED_BACKEND"},
            {
                "status": gone.get("status"),
                "code": (gone.get("error") or {}).get("code"),
                "result": gone.get("result"),
            },
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
                        **(
                            {"engine": args.verify_engine}
                            if args.verify_engine
                            else {}
                        ),
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
                verified.get("status") == "pass"
                and verdict_state == "pass"
                and verified.get("fidelity") == "art-reference"
                and (verdict or {}).get("engine") == "art",
                {
                    "status": verified.get("status"),
                    "fidelity": verified.get("fidelity"),
                    "verdict": verdict_state,
                    "engine": None if verdict is None else verdict.get("engine"),
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
