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

# Seconds a bottom-tab press may take to mount its screen (the Analytics screen
# fetches and then draws), polled one `acos_observe_tree` per second.
TAB_WAIT_SECONDS = 20


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


def named_texts(env: dict) -> list[str]:
    """Every **name** the tree reports: node `text`, else its `content_desc`.

    This is the platform's accessible-name composition, i.e. what an agent reads
    off a node and what `acos_tap {"text": ...}` resolves (see
    docs/AGENT_GUIDE.md). React Native pressables are text-less `ViewGroup`s whose
    whole name is the content description, so a tab bar is only visible here.
    """
    out: list[str] = []

    def walk(value):
        if isinstance(value, dict):
            if "text" in value or "content_desc" in value:
                name = value.get("text") or value.get("content_desc") or ""
                if name:
                    out.append(name)
                for key in ("children",):
                    walk(value.get(key))
                return
            for item in value.values():
                walk(item)
        elif isinstance(value, list):
            for item in value:
                walk(item)

    walk(env.get("result", {}))
    return out


def content_desc_nodes(env: dict, needle: str) -> list[dict]:
    """Every node whose `content_desc` contains `needle` (visible ones first).

    The *name* an agent reads is the platform's accessible name: a node's text,
    else its content description. A React Native `TextInput` is the awkward case
    -- its `text` is the placeholder it draws -- so the field is only reachable
    by its explicit `content_desc` ("Search Japanese dictionary"). This helper
    reads that field so the smoke can tap by the value the tree reports instead
    of hard-coding a label.
    """
    found: list[dict] = []

    def walk(value):
        if isinstance(value, dict):
            if "content_desc" in value or "class" in value:
                desc = value.get("content_desc") or ""
                if needle.lower() in desc.lower():
                    found.append(value)
                for item in value.get("children") or []:
                    walk(item)
                return
            for item in value.values():
                walk(item)
        elif isinstance(value, list):
            for item in value:
                walk(item)

    walk(env.get("result", {}))
    found.sort(key=lambda node: node.get("visible") is not True)
    return found


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
        choices=["counter", "gaten", "gaten-search"],
        default="counter",
        help="counter: 0 -> tap Increment -> 1 -> reset -> 0; gaten: launch, read the "
        "real RN tree, tap a bottom tab and require the tree to change; "
        "gaten-search: focus the search field by its `content_desc`, type with "
        "`acos_text`, and require the query/results in the next tree",
    )
    ap.add_argument(
        "--search-text",
        default="kayou",
        help="gaten-search mode: text to type into the search field",
    )
    ap.add_argument(
        "--search-content-desc",
        default="Search Japanese dictionary",
        help="gaten-search mode: the field's content description, as read from "
        "the tree (the RN TextInput's text is its placeholder, so this is the "
        "explicit selector)",
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

        # REL3b F12: `acos_help` with no topic returns the short overview
        # (REL3b, F12); the whole guide is one `topic: "all"` away. Both are
        # checked, so a shipped-but-empty guide cannot pass either.
        help_env = envelope(client.call_tool("acos_help", {}))
        overview = help_env.get("result", {}).get("guide") or ""
        full_env = envelope(client.call_tool("acos_help", {"topic": "all"}))
        guide = full_env.get("result", {}).get("guide") or ""
        check(
            "acos_help",
            help_env.get("status") == "pass"
            and 1000 < len(overview) < 6000
            and full_env.get("status") == "pass"
            and len(guide) > 20000
            and "acos_tap" in guide,
            {"overview_chars": len(overview), "full_chars": len(guide)},
        )
        full_env = envelope(client.call_tool("acos_help", {"topic": "all"}))
        full_guide = full_env.get("result", {}).get("guide") or ""
        check(
            "acos_help all",
            full_env.get("status") == "pass"
            and len(full_guide) > 20000
            and "acos_tap" in full_guide,
            {"chars": len(full_guide)},
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

        tree0 = envelope(
            client.call_tool(
                "acos_observe_tree", {"session_id": sid}, timeout=args.timeout
            )
        )
        texts0 = tree_texts(tree0)

        if args.mode == "counter":
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
            # A fresh Gaten install boots into onboarding; the bottom tab bar only
            # exists once the three Skip pages are done. Walk there by tapping the
            # onboarding controls **by their text** -- exactly what the tap rule
            # under test has to resolve -- then tap a bottom tab by the label the
            # tree reports (RN pressables carry their whole name in
            # `content_desc`, see docs/AGENT_GUIDE.md "what a `tap` selector
            # resolves to").
            ctas = ["Set up Gaten", "Skip", "Continue", "Open Gaten", "Turn on lookup mode"]
            tab_expect = {
                "Analytics": "Study activity",
                "Settings": "Screen lookup",
                "Home": "Recently looked up",
            }
            for _ in range(6):
                names = named_texts(tree0)
                if any(tab in names for tab in tab_expect) and any(
                    hint in names for hint in tab_expect.values()
                ):
                    break
                cta = next((c for c in ctas if c in names), None)
                check("gaten_onboarding_control_present", cta is not None, {"names": names[:40]})
                stepped = envelope(
                    client.call_tool(
                        "acos_tap", {"session_id": sid, "text": cta}, timeout=args.timeout
                    )
                )
                check(
                    "gaten_onboarding_tap_by_text",
                    stepped.get("status") == "pass"
                    and stepped.get("result", {}).get("accepted") is True,
                    {"text": cta, "status": stepped.get("status"), "error": stepped.get("error")},
                )
                tree0 = envelope(
                    client.call_tool(
                        "acos_observe_tree", {"session_id": sid}, timeout=args.timeout
                    )
                )
                texts0 = tree_texts(tree0)
            names = named_texts(tree0)
            present = [tab for tab in tab_expect if tab in names]
            check(
                "gaten_bottom_tab_present",
                bool(present),
                {"present": present, "names": names[:40]},
            )
            # A tab that is not the current one, so the tap has to navigate.
            tap_text = args.tap_text or next(
                (tab for tab in ("Analytics", "Settings", "Home") if tab in present),
                present[0],
            )

        if args.mode == "gaten-search":
            # FB1: the live channel types. Three claims, each an agent call:
            #  1. the field is named by its `content_desc` (its `text` is the
            #     placeholder), and the tap lands on it;
            #  2. `acos_text` types after that tap -- the same guest `type_text`
            #     command a scenario `type` step sends;
            #  3. the next tree carries the query and the result list, so the
            #     flow is proven by the app's state, never by the call's status.
            fields = content_desc_nodes(tree0, args.search_content_desc)
            check(
                "gaten_search_field_present",
                tree0.get("status") == "pass" and bool(fields),
                {
                    "content_desc": args.search_content_desc,
                    "matched": [f.get("text") for f in fields[:5]],
                    "texts": texts0[:40],
                },
            )
            field = fields[0]
            focused_tap = envelope(
                client.call_tool(
                    "acos_tap",
                    {"session_id": sid, "content_desc": field["content_desc"]},
                    timeout=args.timeout,
                )
            )
            tap_result = focused_tap.get("result") or {}
            settle = tap_result.get("settle") or {}
            # A field tap moves focus and opens the keyboard on the *next* frame,
            # so `state_changed` (the settled tree/pixel verdict) can be false
            # while the gesture was delivered and a change was already measured
            # (`settle.first_change_ms`). The claim here is delivery + change;
            # the proof that it landed *on this field* is the typed query in the
            # next tree (`gaten_search_results_after_typing`), never this flag.
            check(
                "gaten_search_tap_by_content_desc",
                focused_tap.get("status") == "pass"
                and tap_result.get("accepted") is True
                and (
                    tap_result.get("state_changed") is True
                    or settle.get("first_change_ms") is not None
                    or settle.get("frame_ms") is not None
                ),
                {
                    "content_desc": field["content_desc"],
                    "status": focused_tap.get("status"),
                    "accepted": tap_result.get("accepted"),
                    "state_changed": tap_result.get("state_changed"),
                    "settle": {
                        "bound": settle.get("bound"),
                        "first_change_ms": settle.get("first_change_ms"),
                        "frame_ms": settle.get("frame_ms"),
                    },
                    "error": focused_tap.get("error"),
                },
            )
            typed = envelope(
                client.call_tool(
                    "acos_text",
                    {"session_id": sid, "text": args.search_text},
                    timeout=args.timeout,
                )
            )
            check(
                "acos_text_live",
                typed.get("status") == "pass"
                and typed.get("result", {}).get("action") == "text"
                and typed.get("result", {}).get("accepted") is True
                and (typed.get("result", {}).get("target") or {}).get("text")
                == args.search_text,
                {
                    "text": args.search_text,
                    "status": typed.get("status"),
                    "accepted": typed.get("result", {}).get("accepted"),
                    "target": typed.get("result", {}).get("target"),
                    "error": typed.get("error"),
                },
            )
            # The query and the result list mount on later frames: poll the tree
            # (bounded, never forever) for both. A field that never shows the
            # query, or a list that never arrives, still fails.
            result_markers = ("results", "通う", "歌謡")
            typed_texts = tree_texts(
                envelope(
                    client.call_tool(
                        "acos_observe_tree", {"session_id": sid}, timeout=args.timeout
                    )
                )
            )
            for _ in range(TAB_WAIT_SECONDS):
                if args.search_text in typed_texts and any(
                    marker in text for text in typed_texts for marker in result_markers
                ):
                    break
                time.sleep(1)
                typed_texts = tree_texts(
                    envelope(
                        client.call_tool(
                            "acos_observe_tree", {"session_id": sid}, timeout=args.timeout
                        )
                    )
                )
            check(
                "gaten_search_results_after_typing",
                args.search_text in typed_texts
                and any(
                    marker in text for text in typed_texts for marker in result_markers
                ),
                {"query": args.search_text, "texts": typed_texts[:40]},
            )

        if tap_text is None and args.mode != "gaten-search":
            raise SmokeError("no node text to tap")
        tap_args = None
        if args.mode == "gaten":
            # Tap by the label read off the tree, never by bounds: this is the
            # claim (an RN tab is reachable by the name an agent reads).
            tap_args = {"session_id": sid, "text": tap_text}
        elif args.mode == "counter":
            tap_args = {"session_id": sid, "text": tap_text}
        tapped = (
            envelope(client.call_tool("acos_tap", tap_args, timeout=args.timeout))
            if tap_args is not None
            else None
        )
        if tapped is not None:
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
        if args.mode == "gaten" and tapped is not None:
            check(
                "gaten_tab_tap_state_changed",
                tapped.get("result", {}).get("state_changed") is True,
                {"text": tap_text, "result": tapped.get("result", {}).get("settle")},
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
        elif args.mode == "gaten-search":
            # The search flow already proved its own state (`acos_text_live` +
            # `gaten_search_results_after_typing`); nothing else is claimed here.
            pass
        else:
            if tap_text in tab_expect:
                # A tab press navigates and the new screen mounts on its own
                # frame: poll the tree (bounded, never forever) for the tab's own
                # content. A screen that never arrives still fails.
                expected = tab_expect[tap_text]
                names1 = named_texts(tree1)
                for _ in range(TAB_WAIT_SECONDS):
                    if expected in names1 and tap_text in names1:
                        break
                    time.sleep(1)
                    tree1 = envelope(
                        client.call_tool(
                            "acos_observe_tree", {"session_id": sid}, timeout=args.timeout
                        )
                    )
                    texts1 = tree_texts(tree1)
                    names1 = named_texts(tree1)
                check(
                    "gaten_tab_screen_text",
                    tree1.get("status") == "pass"
                    and tap_text in names1
                    and expected in names1
                    and texts1 != texts0,
                    {
                        "tab": tap_text,
                        "expected_text": expected,
                        "before": texts0[:40],
                        "after": texts1[:40],
                    },
                )
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
