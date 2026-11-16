#!/usr/bin/env python3
"""REL3K2 item 2: one live `acos-mcp` run on a real ART session.

Drives the MCP server exactly as an agent would (newline-delimited JSON-RPC on
stdio), on the ART engine (the MCP server's own private `acos serve --engine
art` daemon), and checks the Back contract through the MCP boundary:

    acos_launch (multi-activity) -> open SecondActivity (tap Send)
    -> acos_key {code:"back"}   : handled:true, and the first Activity is shown
    -> acos_key {code:"back"}   : the task is finished (window-gone)

Every request/response frame is printed as one JSON line, then one
`{"check": ...}` line per assertion and a final `{"result": "ok"|"fail"}` line.
Exit status is 0 only when every check passed.

Usage (under run-exclusive + guarded):
  tools/art-runtime/run-exclusive.sh --task rel3k2 -- tools/guarded.sh \
      --mem 14G --time 900 -- env ACOS_ART_BIN=<build>/acos-art \
      python3 results/handoff/REL3K2/mcp_art_back.py --mcp-bin <build>/acos-mcp
"""

from __future__ import annotations

import argparse
import json
import os
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[3]
sys.path.insert(0, str(ROOT / "tools" / "release"))

import mcp_smoke  # noqa: E402  (the release smoke client: MCP stdio + envelope)

Mcp = mcp_smoke.Mcp
envelope = mcp_smoke.envelope
emit = mcp_smoke.emit
check = mcp_smoke.check


def texts(node_lists) -> list[str]:
    out: list[str] = []
    for nodes in node_lists:
        for node in nodes or []:
            for key in ("text", "content_desc"):
                value = node.get(key)
                if isinstance(value, str) and value:
                    out.append(value)
    return out


def compact_texts(env: dict) -> list[str]:
    result = env.get("result") or {}
    nodes = result.get("nodes")
    if nodes is None:
        nodes = (result.get("tree") or {}).get("nodes")
    return texts([nodes])


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--mcp-bin", required=True)
    ap.add_argument("--apk", default=str(ROOT / "fixtures/build/apps/multi-activity.apk"))
    ap.add_argument("--timeout", type=float, default=180.0)
    args = ap.parse_args()

    env = dict(os.environ)
    env.pop("ACOS_HOME", None)
    env.setdefault(
        "ACOS_FONT_DIR", "/home/Qui/Projects/acos/.cache/boot/android-34/fonts"
    )
    client = Mcp(Path(args.mcp_bin), env)
    ok = True
    try:
        init = client.request(
            "initialize",
            {
                "protocolVersion": mcp_smoke.PROTOCOL_VERSION,
                "capabilities": {},
                "clientInfo": {"name": "rel3k2-art-back", "version": "1"},
            },
        )
        client.notify("notifications/initialized")
        emit({"check": "initialize", "ok": True, "detail": init.get("serverInfo")})

        tools = client.request("tools/list")
        names = [t.get("name") for t in tools.get("tools", [])]
        check("acos_key_is_advertised", "acos_key" in names, names)

        launch = envelope(
            client.call_tool(
                "acos_launch",
                {"apk": args.apk},
                timeout=args.timeout,
            )
        )
        emit({"check": "launch", "ok": launch.get("ok") is True,
              "detail": {"status": launch.get("status"),
                         "reached_stage": (launch.get("result") or {}).get("reached_stage"),
                         "engine": (launch.get("result") or {}).get("engine")}})
        check("launch_is_art", (launch.get("result") or {}).get("engine") == "art",
              launch.get("result", {}).get("engine"))
        # The MCP launch reply names the rung `stage_reached` (the CLI's
        # `reached_stage`), and carries the frame under `first_frame`.
        reached = (launch.get("result") or {}).get("stage_reached")
        check("launch_reached_first_frame", reached == "first_frame", reached)

        tree = envelope(client.call_tool("acos_observe_tree", {}, timeout=args.timeout))
        first = compact_texts(tree)
        check("first_activity_is_shown", "Send" in first, first)

        # Open the second Activity through the app's own button.
        tap = envelope(
            client.call_tool("acos_tap", {"id": "send"}, timeout=args.timeout)
        )
        check("tap_send_handled", tap.get("ok") is True, tap.get("error"))
        tree = envelope(client.call_tool("acos_observe_tree", {}, timeout=args.timeout))
        second = compact_texts(tree)
        check("second_activity_is_shown", any(t.startswith("Received") for t in second), second)

        # ---- Back #1: pops SecondActivity, MainActivity is shown again -----
        back1 = envelope(
            client.call_tool("acos_key", {"code": "back"}, timeout=args.timeout)
        )
        r1 = back1.get("result") or {}
        emit({"check": "back1_reply", "ok": True,
              "detail": {k: r1.get(k) for k in
                         ("engine", "handled", "handled_evidence", "settle", "task_finished",
                          "window_gone", "pixels_changed", "current_activity")}})
        check("back1_is_pass", back1.get("ok") is True, back1.get("error"))
        check("back1_handled_true", r1.get("handled") is True, r1.get("handled"))
        check("back1_does_not_finish_task",
              not r1.get("task_finished") and not r1.get("window_gone"),
              {"task_finished": r1.get("task_finished"), "window_gone": r1.get("window_gone")})
        tree = envelope(client.call_tool("acos_observe_tree", {}, timeout=args.timeout))
        after1 = compact_texts(tree)
        check("back1_shows_first_activity_again", "Send" in after1, after1)
        check("back1_second_activity_gone",
              not any(t.startswith("Received") for t in after1), after1)

        # ---- Back #2: the root Activity finishes the task -------------------
        back2 = envelope(
            client.call_tool("acos_key", {"code": "back"}, timeout=args.timeout)
        )
        r2 = back2.get("result") or {}
        emit({"check": "back2_reply", "ok": True,
              "detail": {k: r2.get(k) for k in
                         ("engine", "handled", "handled_evidence", "settle", "task_finished",
                          "window_gone", "pixels_changed")}})
        settle_bound = (r2.get("settle") or {}).get("bound")
        finished = bool(r2.get("task_finished")) or bool(r2.get("window_gone")) or (
            settle_bound == "window-gone")
        check("back2_reports_task_finished", finished,
              {"task_finished": r2.get("task_finished"),
               "window_gone": r2.get("window_gone"), "settle_bound": settle_bound})

        destroy = envelope(client.call_tool("acos_destroy", {}, timeout=args.timeout))
        check("destroy", destroy.get("ok") is True, destroy.get("error"))
    except Exception as exc:  # noqa: BLE001 - the transcript is the evidence
        ok = False
        emit({"check": "driver", "ok": False, "detail": f"{type(exc).__name__}: {exc}"})
    finally:
        client.close()
    emit({"result": "ok" if ok else "fail"})
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())
