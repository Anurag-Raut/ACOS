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
  * `--mode weak` (BINDERREF2 §2a): registers N listeners, then taps "Weaken
    listeners" — the app moves every listener into a `WeakReference` list, drops
    the strong references and forces a collection — then injects the ringing
    event. The app reports how many *Java objects* survived the GC (`CALLWEAK
    ... alive=`) and how many ran their callback (`CALLTOTAL ... ringing=`). On
    Android the two numbers agree (the host's strong reference on the callback
    binder keeps the `JavaBBinder`, and through its JNI global reference the Java
    object, alive); the gap between them is the measured §2a behaviour.

    BINDERREF4 adds the stub counter to that line (`stubs=`): the listener is
    collected by the framework's own `WeakReference` (AOSP-correct), while the
    callback *binder* is the object the rooted node must keep alive.

  * `--mode stub` (BINDERREF4 §1/§3): the probe arms. One listener the app keeps
    in a static field (`Probe strong stub`), N listeners the app does not keep
    (`Probe detached stub`), and a broadcast receiver the app does not keep
    (`Probe detached receiver`), then two `System.gc()` calls, an idle, the
    scripted ring and the scripted SMS. Asserts the strong arm ran, the detached
    arm's stubs are still alive (the JavaBBinder edge) with their listeners
    collected (AOSP), and the detached receiver ran after the GC.

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
    parser.add_argument("--mode", choices=["longlived", "unregister", "repeat", "weak", "detach",
                                           "stub", "biometric"],
                        required=True)
    parser.add_argument("--registers", type=int, default=0,
                        help="repeat mode: how many extra 'Listen for calls' registrations")
    parser.add_argument("--idle-s", type=float, default=65.0)
    parser.add_argument("--artifacts", required=True)
    parser.add_argument("--out", required=True)
    parser.add_argument("--timeout-s", type=float, default=1800.0)
    parser.add_argument("--min-priority", default=None,
                        help="acos_logs min_priority (default: the MCP default, W)")
    args = parser.parse_args()

    evidence = {"mode": args.mode, "idle_s": args.idle_s, "steps": []}
    mcp = Mcp(args.binary)
    failures = []
    try:
        services = [{"id": "net.telephony", "profile": "scripted"}]
        if args.mode == "biometric":
            # The biometric prompt's callback is the last module callback site
            # (BINDERREF3): the receiver must be referenced while the request is
            # pending, and reachable after the app stopped holding its callback.
            services = [{"id": "hw.biometric", "profile": "scripted"}]
        launch = call_payload(mcp.call("acos_launch", {
            "apk": os.path.abspath(args.apk),
            "session_id": f"bindercb-{args.mode}",
            "label": f"bindercb-{args.mode}",
            "services": services,
            "pixels": False,
        }))
        session_id = launch.get("session_id") or f"bindercb-{args.mode}"
        evidence["steps"].append({"launch": launch.get("ok"), "session_id": session_id,
                                  "reached_stage": launch.get("reached_stage"),
                                  "error": launch.get("error")})
        if not launch.get("ok", True) and launch.get("error"):
            failures.append(f"launch failed: {launch['error']}")

        # The listener registered in onCreate got the registry's initial state.
        text = observe_text(mcp, session_id, evidence["steps"])
        if args.mode != "biometric" and "CALL state=IDLE" not in text:
            failures.append("the onCreate listener never received the registry's initial state")

        if args.mode == "biometric":
            # Register (the callback binder becomes a pending request), force a
            # collection, idle, then answer from the host: the answer is a module
            # callback into the app, so the node must still be rooted.
            for label in ("Biometric", "Authenticate"):
                tap = call_payload(mcp.call("acos_tap", {
                    "session_id": session_id,
                    "text": label,
                    "diff": False,
                }))
                evidence["steps"].append({"tap": label, "ok": tap.get("ok", True)})
                if not tap.get("ok", True):
                    failures.append(f"tap '{label}' failed: {tap.get('error')}")
                    break
                time.sleep(0.5)
            evidence["steps"].append({"tap": "Authenticate", "ok": tap.get("ok", True)})
            time.sleep(1.0)
            answer = call_payload(mcp.call("acos_service_ctl", {
                "base": {"session_id": session_id},
                "module": "hw.biometric",
                "op": "auth",
                "args": {"result": "success"},
            }))
            evidence["steps"].append({"service_ctl": "auth", "reply": answer})
            evidence["ctl_reply"] = answer
            time.sleep(args.idle_s)
            text = observe_text(mcp, session_id, evidence["steps"])
            if "AUTH:SUCCESS" not in text:
                failures.append(
                    "the biometric receiver did not get the scripted success after the "
                    "pending authenticate (BINDERREF3's pending-receiver reference)")
            evidence["verdict"] = "pass" if not failures else "fail"
            evidence["failures"] = failures
            with open(args.out, "w", encoding="utf-8") as handle:
                json.dump(evidence, handle, indent=2, sort_keys=True)
            print(json.dumps({"mode": args.mode, "verdict": evidence["verdict"],
                              "failures": failures}, indent=2))
            return 0 if not failures else 1

        if args.mode == "unregister":
            tap = call_payload(mcp.call("acos_tap", {
                "session_id": session_id,
                "text": "Stop listening",
            }))
            evidence["steps"].append({"tap": "Stop listening", "ok": tap.get("ok", True)})
            time.sleep(2.0)

        if args.mode in ("repeat", "weak", "detach"):
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

        if args.mode == "weak":
            # The app drops its own references to every registered listener and
            # forces a collection: the registration (and the host's reference on
            # the callback binder) is the only thing left.
            weaken = call_payload(mcp.call("acos_tap", {
                "session_id": session_id,
                "text": "Weaken listeners",
                "diff": False,
            }))
            evidence["steps"].append({"tap": "Weaken listeners", "ok": weaken.get("ok", True)})
            time.sleep(2.0)
            text = observe_text(mcp, session_id, evidence["steps"])
            for line in text.split("\n"):
                if line.startswith("CALLWEAK "):
                    evidence["app_weaken_line"] = line
                    for field in line.split()[1:]:
                        key, _, value = field.partition("=")
                        try:
                            evidence["app_weak_" + key] = int(value)
                        except ValueError:
                            pass

        if args.mode == "stub":
            # The three BINDERREF4 arms, registered from the probe buttons: the
            # app keeps the strong arm's listener, keeps nothing for the
            # detached arm (neither listener nor stub), and keeps nothing for
            # the receiver either (the framework's ReceiverDispatcher does).
            for label in ["Probe strong stub"] + ["Probe detached stub"] * args.registers:
                tap = call_payload(mcp.call("acos_tap", {
                    "session_id": session_id,
                    "text": label,
                    "diff": False,
                }))
                evidence["steps"].append({"tap": label, "ok": tap.get("ok", True)})
                if not tap.get("ok", True):
                    failures.append(f"tap '{label}' failed: {tap.get('error')}")
            tap = call_payload(mcp.call("acos_tap", {
                "session_id": session_id,
                "text": "Probe detached receiver",
                "diff": False,
            }))
            evidence["steps"].append({"tap": "Probe detached receiver",
                                      "ok": tap.get("ok", True)})
            time.sleep(1.0)
            # Two forced collections with finalization in between; the probe
            # button does both `System.gc()` calls, and the app reports what the
            # GC left alive (listeners and callback stubs separately).
            tap = call_payload(mcp.call("acos_tap", {
                "session_id": session_id,
                "text": "Probe GC",
                "diff": False,
            }))
            evidence["steps"].append({"tap": "Probe GC", "ok": tap.get("ok", True)})
            time.sleep(2.0)
            text = observe_text(mcp, session_id, evidence["steps"])
            for line in text.split("\n"):
                if line.startswith("PROBEGC "):
                    evidence["probe_gc_line"] = line
                    for field in line.split()[1:]:
                        key, _, value = field.partition("=")
                        try:
                            evidence["gc_" + key] = int(value)
                        except ValueError:
                            pass
            # The guest-side rooting trace (item 1): `node: local binder=… ->
            # wire=…` is the export and `node refs: BR_INCREFS+BR_ACQUIRE` is
            # libbinder taking the strong+weak reference on the `JavaBBinder`,
            # i.e. the edge that keeps the Java object alive while the node is
            # rooted. Captured here, right after the probe registrations, because
            # `acos_logs` returns the session's tail and a launch's first exports
            # are earlier than the window.
            tap_logs = call_payload(mcp.call("acos_logs", {
                "session_id": session_id,
                "tag": "acos-binder",
                "all": True,
            }))
            tap_result = tap_logs.get("result") if isinstance(tap_logs, dict) else None
            evidence["binder_log_at_probe"] = [
                (r.get("line") if isinstance(r, dict) else str(r))
                for r in ((tap_result or {}).get("logs") or [])]

            # The second half of the detached-receiver arm: a broadcast the
            # registration (not the app) keeps the receiver for. It is injected
            # after the forced GC, so a delivery proves the node stayed rooted.
            sms = call_payload(mcp.call("acos_service_ctl", {
                "base": {"session_id": session_id},
                "module": "net.telephony",
                "op": "sms",
                "args": {"from": "+15550003333", "body": "binderref4 probe"},
            }))
            evidence["steps"].append({"service_ctl": "sms", "reply": sms})
            evidence["sms_reply"] = sms
            time.sleep(2.0)

        if args.mode == "detach":
            # BINDERREF2 item 2: a module detach must release every node its
            # registrations rooted. The session stays alive, so a node that is
            # still rooted after the detach is a leak the driver trace shows.
            detach = call_payload(mcp.call("acos_services", {
                "session_id": session_id,
                "op": "detach",
                "id": "net.telephony",
            }))
            evidence["steps"].append({"services_detach": detach})
            evidence["detach_reply"] = detach
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
            "base": {"session_id": session_id},
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

        # Ask the app itself, from its UI thread, how many callbacks its Java
        # code ran: logd lines and TextView appends are written from binder
        # threads, so a harness that counts only those cannot tell a dropped
        # transaction from a dropped log line.
        tap_report = call_payload(mcp.call("acos_tap", {
            "session_id": session_id,
            "text": "Report callbacks",
            "diff": False,
        }))
        evidence["steps"].append({"tap": "Report callbacks", "ok": tap_report.get("ok", True)})
        time.sleep(1.0)
        text = observe_text(mcp, session_id, evidence["steps"])
        for line in text.split("\n"):
            if line.startswith("CALLTOTAL "):
                for field in line.split()[1:]:
                    key, _, value = field.partition("=")
                    try:
                        evidence["app_" + key] = int(value)
                    except ValueError:
                        pass

        evidence["ringing_after"] = after
        evidence["tree_text_after"] = text
        log_args = {"session_id": session_id, "tag": "acos-binder"}
        if args.min_priority:
            log_args["min_priority"] = args.min_priority
        logs = call_payload(mcp.call("acos_logs", log_args))
        app_logs = call_payload(mcp.call("acos_logs", {"session_id": session_id,
                                                       "tag": "tailprobe",
                                                       "min_priority": "I"}))
        app_result = app_logs.get("result") if isinstance(app_logs, dict) else None
        app_records = (app_result or {}).get("logs") or []
        app_lines = [(r.get("message") if isinstance(r, dict) else str(r))
                     for r in app_records]
        evidence["app_log"] = app_logs
        evidence["app_log_lines"] = app_lines
        evidence["dispatched_ringing"] = sum(
            1 for line in app_lines if line and "CALL state=RINGING" in line)
        evidence["dispatched_idle"] = sum(
            1 for line in app_lines if line and "CALL state=IDLE" in line)
        # Everything the guest said at W and above (libbinder's own errors
        # included): a "Cannot transact call to unmanaged code" here, or its
        # absence, decides whether a delivery reached a dead weakref.
        all_logs = call_payload(mcp.call("acos_logs", {"session_id": session_id,
                                                       "min_priority": "W"}))
        all_result = all_logs.get("result") if isinstance(all_logs, dict) else None
        evidence["guest_log_lines"] = [
            (r.get("line") if isinstance(r, dict) else str(r))
            for r in ((all_result or {}).get("logs") or [])]
        evidence["binder_logs"] = logs
        result = logs.get("result") if isinstance(logs, dict) else None
        records = (result or logs).get("logs") if isinstance(result or logs, dict) else None
        evidence["binder_log_lines"] = [
            (r.get("line") if isinstance(r, dict) else str(r)) for r in (records or [])]

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
            dispatched = evidence.get("app_ringing")
            if dispatched is None:
                dispatched = evidence.get("dispatched_ringing") or 0
            if dispatched < expected:
                failures.append(
                    f"only {dispatched} of {expected} registered PhoneStateListeners were "
                    "dispatched the scripted ringing event (logd count)")
            # The per-line UI count is a *lower bound* once the app's log view
            # holds more lines than the tree dumper emits for one TextView (a
            # single `TextView` is capped: at 101 listeners the dump carries 82
            # of the 202 CALL lines). The authoritative UI-visible number is the
            # `CALLTOTAL` line the app prints from its own counter.
            evidence["ui_ringing_lines"] = after - before
            if after - before > expected:
                failures.append(
                    f"{after - before} ringing lines for {expected} listeners (duplicate "
                    "delivery?)")
        elif args.mode == "detach":
            # The trace must show the driver unrooting the nodes the module's
            # registrations held (`node release wire=…` from the peer's F_REF,
            # then `node unref: BR_RELEASE + BR_DECREFS` on the guest).
            released = [l for l in evidence.get("binder_log_lines", [])
                        if "node release" in l or "node unref:" in l]
            evidence["node_release_lines"] = released
            if not released:
                failures.append(
                    "module detach left the registrations' nodes rooted: no "
                    "`node release`/`node unref` line in the driver trace")
            # The module is gone, so the injected state has nowhere to go.
            evidence["detach_ringing"] = after - before
            if after != before:
                failures.append("a callback ran after the module detached")
        elif args.mode == "stub":
            # The acceptance: the callback stub the app holds must be called;
            # the stub the app dropped but the registration keeps must still be
            # *alive* (the libbinder JavaBBinder edge), with its listener
            # collected (AOSP's own `WeakReference`); and the detached receiver
            # must have run after the GC.
            evidence["probe"] = {
                "gc": evidence.get("probe_gc_line"),
                "strong_ringing": evidence.get("app_probe_strong_ringing"),
                "detached_registered": evidence.get("app_probe_detached"),
                "detached_listeners": evidence.get("app_probe_detached_listeners"),
                "detached_stubs": evidence.get("app_probe_detached_stubs"),
                "detached_ringing": evidence.get("app_probe_detached_ringing"),
            }
            probe_calls = [l for l in evidence.get("app_log_lines", [])
                           if l and l.startswith("PROBECALL ")]
            evidence["probe_calls"] = probe_calls
            strong_calls = [l for l in probe_calls if "arm=strong" in l]
            if not strong_calls:
                failures.append(
                    "the strong arm's callback stub (held in a static field) was not "
                    "called after the forced GC")
            expected_detached = evidence.get("app_probe_detached") or 0
            stubs = evidence.get("app_probe_detached_stubs")
            listeners_alive = evidence.get("app_probe_detached_listeners")
            if stubs is None:
                failures.append("the app did not report the detached stub counters (CALLTOTAL)")
            elif stubs < expected_detached:
                failures.append(
                    f"{expected_detached - stubs} detached callback stub(s) were collected "
                    "while their node was still rooted by the registration "
                    f"(alive stubs={stubs}/{expected_detached}) -- the JavaBBinder "
                    "global-reference edge is missing")
            if listeners_alive:
                failures.append(
                    f"{listeners_alive} detached PhoneStateListener object(s) survived the "
                    "GC; AOSP's stub holds only a WeakReference to the listener, so a "
                    "dropped listener must be collected")
            if evidence.get("app_probe_detached_ringing"):
                failures.append(
                    "a *ringing* callback ran for a detached PhoneStateListener whose Java "
                    "object AOSP would have collected")
            trace = evidence.get("binder_log_at_probe", []) + evidence.get("binder_log_lines", [])
            evidence["root_trace_lines"] = [l for l in trace
                                            if "node: local binder=" in l
                                            or "node refs:" in l]
            # The export/root lines are written at registration time; whether
            # they are inside `acos_logs`' window depends on how much traffic
            # followed. Their presence is recorded as evidence, and their
            # absence is called out in the report instead of failing the probe:
            # the gate is the Java-object aliveness above, which does not depend
            # on the trace.
            evidence["root_trace_captured"] = bool(evidence["root_trace_lines"])
            if not any("PROBESMS " in l for l in evidence.get("app_log_lines", [])):
                failures.append(
                    "the detached broadcast receiver did not run after the forced GC: "
                    "the registration no longer keeps its node alive")
        elif args.mode == "weak":
            # §2a is a *measurement*, not an acceptance gate: the run records the
            # number of Java listener objects the GC left alive and the number
            # whose callback ran. `verdict` is pass when every *alive* listener
            # ran (nothing that still exists was dropped) and the run says so.
            expected = 1 + evidence.get("registered", 0)
            evidence["listeners_expected"] = expected
            alive = evidence.get("app_weak_alive")
            dispatched = evidence.get("app_ringing")
            evidence["weak_dispatched"] = dispatched
            if alive is not None and dispatched is not None and dispatched > alive:
                failures.append(
                    f"{dispatched} callbacks ran but only {alive} listener objects were "
                    "alive (a callback ran on a collected object)")
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
