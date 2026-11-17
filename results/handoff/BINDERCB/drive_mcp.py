#!/usr/bin/env python3
"""BINDERCB: drive a live ART session through the acos MCP server (JSON-RPC on
stdio) so a host->guest callback can be injected *long after* registration.

Why not a scenario? `acos verify` scenarios are event-driven and have no sleep
step, so they cannot express "register at app start, idle for >= 60 s, then
inject". This driver launches the tail-probe APK once, keeps the session alive,
and:

  * `--mode longlived`: waits 65 s after the launch, then injects the scripted
    ringing call state and asserts the PhoneStateListener that was registered in
    `onCreate` printed `CALL state=RINGING number=+15550002222`.
  * `--mode unregister`: waits 65 s, taps "Stop listening" (the app calls
    `telephony.listen(listener, LISTEN_NONE)`), waits 2 s, injects the same
    event and asserts the listener did NOT fire again (the app's own log must
    stay at its pre-event count).

Nothing is faked: every assertion reads the app's own printed lines out of the
live UI tree, and the run's JSON (session/ctl replies, tree text before and
after) is written to `--out`.
"""
import argparse
import json
import os
import subprocess
import sys
import time

HERE = os.path.dirname(os.path.abspath(__file__))


class Mcp:
    def __init__(self, binary):
        self.proc = subprocess.Popen(
            [binary],
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            bufsize=1,
        )
        self.next_id = 1
        self.request("initialize", {
            "protocolVersion": "2025-06-18",
            "capabilities": {},
            "clientInfo": {"name": "bindercb-driver", "version": "1"},
        })
        self.notify("notifications/initialized", {})

    def send(self, payload):
        assert self.proc.stdin is not None
        self.proc.stdin.write(json.dumps(payload) + "\n")
        self.proc.stdin.flush()

    def notify(self, method, params):
        self.send({"jsonrpc": "2.0", "method": method, "params": params})

    def request(self, method, params, timeout=1800):
        request_id = self.next_id
        self.next_id += 1
        self.send({"jsonrpc": "2.0", "id": request_id, "method": method, "params": params})
        deadline = time.time() + timeout
        assert self.proc.stdout is not None
        while time.time() < deadline:
            line = self.proc.stdout.readline()
            if not line:
                raise RuntimeError("MCP server closed stdout")
            try:
                message = json.loads(line)
            except json.JSONDecodeError:
                continue
            if message.get("id") == request_id:
                if "error" in message:
                    raise RuntimeError(f"{method}: {message['error']}")
                return message["result"]
        raise RuntimeError(f"{method}: timed out after {timeout}s")

    def call(self, tool, arguments, timeout=1800):
        return self.request("tools/call", {"name": tool, "arguments": arguments}, timeout)

    def close(self):
        try:
            if self.proc.stdin is not None:
                self.proc.stdin.close()
            self.proc.wait(timeout=120)
        except Exception:
            self.proc.kill()


def text_of(result):
    """The union of every text node in a tool reply (tree lines included)."""
    chunks = []

    def walk(value):
        if isinstance(value, dict):
            for key, item in value.items():
                if key in ("text", "content_description") and isinstance(item, str):
                    chunks.append(item)
                else:
                    walk(item)
        elif isinstance(value, list):
            for item in value:
                walk(item)
        elif isinstance(value, str):
            chunks.append(value)

    walk(result)
    return "\n".join(chunks)


def call_payload(response):
    """Unwrap an MCP `tools/call` reply into its JSON payload."""
    if isinstance(response, dict) and "structuredContent" in response:
        content = response["structuredContent"]
        if isinstance(content, dict) and set(content.keys()) == {"result"}:
            return content["result"]
        return content
    if isinstance(response, dict) and "content" in response:
        for item in response["content"]:
            if item.get("type") == "text":
                try:
                    return json.loads(item["text"])
                except json.JSONDecodeError:
                    return {"text": item["text"]}
    return response


def observe_text(mcp, session_id, out):
    reply = call_payload(mcp.call("acos_observe_tree", {"session_id": session_id}))
    text = text_of(reply)
    out.append({"observe": "tree", "ok": reply.get("ok", reply.get("status")), "text": text})
    return text


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--binary", default=os.path.join(HERE, "..", "..", "..",
                                                         ".cache/bindercb-target/release/acos-mcp"))
    parser.add_argument("--apk", required=True)
    parser.add_argument("--mode", choices=["longlived", "unregister", "repeat"], required=True)
    parser.add_argument("--registers", type=int, default=0,
                        help="repeat mode: how many extra 'Listen for calls' registrations")
    parser.add_argument("--idle-s", type=float, default=65.0)
    parser.add_argument("--artifacts", required=True)
    parser.add_argument("--out", required=True)
    parser.add_argument("--timeout-s", type=float, default=1800.0)
    args = parser.parse_args()

    evidence = {"mode": args.mode, "idle_s": args.idle_s, "steps": []}
    mcp = Mcp(args.binary)
    failures = []
    try:
        launch = call_payload(mcp.call("acos_launch", {
            "apk": os.path.abspath(args.apk),
            "session_id": f"bindercb-{args.mode}",
            "label": f"bindercb-{args.mode}",
            "services": [{"id": "net.telephony", "profile": "scripted"}],
            "pixels": False,
            "artifacts_dir": os.path.abspath(args.artifacts),
        }))
        session_id = launch.get("session_id") or f"bindercb-{args.mode}"
        evidence["steps"].append({"launch": launch.get("ok"), "session_id": session_id,
                                  "reached_stage": launch.get("reached_stage"),
                                  "error": launch.get("error")})
        if not launch.get("ok", True) and launch.get("error"):
            failures.append(f"launch failed: {launch['error']}")

        # The listener registered in onCreate got the registry's initial state.
        text = observe_text(mcp, session_id, evidence["steps"])
        if "CALL state=IDLE" not in text:
            failures.append("the onCreate listener never received the registry's initial state")

        if args.mode == "unregister":
            tap = call_payload(mcp.call("acos_tap", {
                "session_id": session_id,
                "text": "Stop listening",
            }))
            evidence["steps"].append({"tap": "Stop listening", "ok": tap.get("ok", True)})
            time.sleep(2.0)

        if args.mode == "repeat":
            # Each tap creates a *new* PhoneStateListener (a new Java Binder, and
            # so a new driver node). This is the node-lifetime pressure case:
            # registrations keep arriving for the whole session and the host
            # keeps a reference to every one of them, exactly like an app that
            # subscribes per screen.
            registered = 0
            for _ in range(args.registers):
                tap = call_payload(mcp.call("acos_tap", {
                    "session_id": session_id,
                    "text": "Listen for calls",
                    "diff": False,
                }))
                if not tap.get("ok", True):
                    failures.append(f"tap 'Listen for calls' failed: {tap.get('error')}")
                    break
                registered += 1
            evidence["registered"] = registered
            time.sleep(2.0)

        # The long-lived part: the session idles for >= 60 s with the listener
        # registered and no binder traffic aimed at it.
        time.sleep(args.idle_s)

        text = observe_text(mcp, session_id, evidence["steps"])
        before = text.count("CALL state=RINGING")
        evidence["ringing_before"] = before

        status = call_payload(mcp.call("acos_services", {
            "session_id": session_id,
            "op": "status",
        }))
        evidence["steps"].append({"services_status": status})
        ctl_raw = mcp.call("acos_service_ctl", {
            "session_id": session_id,
            "module": "net.telephony",
            "op": "call_state",
            "args": {"state": "ringing", "number": "+15550002222"},
        })
        ctl = call_payload(ctl_raw)
        evidence["steps"].append({"service_ctl": ctl_raw.get("ok", ctl.get("ok")), "reply": ctl})
        evidence["ctl_reply"] = ctl
        delivered = None
        if isinstance(ctl, dict):
            delivered = ctl.get("delivered")
            if delivered is None and isinstance(ctl.get("value"), dict):
                delivered = ctl["value"].get("delivered")
            if delivered is None and isinstance(ctl.get("result"), dict):
                delivered = ctl["result"].get("delivered")
        evidence["host_delivered"] = delivered

        # Give the guest a moment to dispatch the callback, then read the tree.
        deadline = time.time() + 30.0
        after = before
        while time.time() < deadline:
            time.sleep(1.0)
            text = observe_text(mcp, session_id, evidence["steps"])
            after = text.count("CALL state=RINGING")
            if after != before:
                break

        evidence["ringing_after"] = after
        evidence["tree_text_after"] = text
        logs = call_payload(mcp.call("acos_logs", {"session_id": session_id,
                                                   "tag": "acos-binder"}))
        evidence["binder_logs"] = logs
        if isinstance(logs, dict):
            records = logs.get("records") or logs.get("entries") or []
            evidence["binder_log_lines"] = [
                (r.get("line") if isinstance(r, dict) else str(r)) for r in records]

        if args.mode == "longlived":
            if after <= before:
                failures.append(
                    "the long-lived PhoneStateListener registered in onCreate was NOT "
                    f"called after {args.idle_s:.0f}s idle (host queued {delivered} delivery)")
            elif "CALL state=RINGING number=+15550002222" not in text:
                failures.append("the listener fired but without the scripted number")
        elif args.mode == "repeat":
            # Every registered listener (the onCreate one plus one per tap) must
            # be called: a registration the driver cannot address any more is a
            # callback the framework dropped.
            expected = 1 + evidence.get("registered", 0)
            evidence["listeners_expected"] = expected
            if after - before < expected:
                failures.append(
                    f"only {after - before} of {expected} registered PhoneStateListeners were "
                    "called for the scripted ringing event")
        else:
            if after != before:
                failures.append(
                    "the listener was called after unregister (LEGACY_CALL_STATE_CHANGED "
                    "still delivered)")
    finally:
        # The MCP server tears its private daemon (and every session) down on
        # stdin EOF, so closing is the teardown path.
        mcp.close()

    evidence["failures"] = failures
    evidence["verdict"] = "pass" if not failures else "fail"
    with open(args.out, "w", encoding="utf-8") as handle:
        json.dump(evidence, handle, indent=2, sort_keys=True)
    print(json.dumps({"mode": args.mode, "verdict": evidence["verdict"],
                      "failures": failures, "host_delivered": evidence.get("host_delivered"),
                      "ringing_before": evidence.get("ringing_before"),
                      "ringing_after": evidence.get("ringing_after")}, indent=2))
    return 0 if not failures else 1


if __name__ == "__main__":
    sys.exit(main())
