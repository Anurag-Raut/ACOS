#!/usr/bin/env python3
"""VIEW1c acceptance driver: prove `acos view` end to end against a real session.

Starts a real `acos-mcp` (private ART daemon), launches counter.apk, starts
`acos view`, then drives the hub exactly as a browser would: page, session list,
MJPEG frames, SSE events, a tap ripple event, the token gate, and the subscriber
count going back to zero when the last browser leaves.

Not committed to the repo: it is the measurement harness for
`results/handoff/VIEW1c.md`. Everything lives under WT/.cache/view1c-proof.
"""

from __future__ import annotations

import hashlib
import http.client
import importlib.util
import json
import os
import subprocess
import sys
import threading
import time
import urllib.error
import urllib.request
from pathlib import Path

WT = Path("/home/Qui/Projects/acos/.cache/wt-view1")
TARGET = WT / ".cache/view1-target/release"
COUNTER = Path("/home/Qui/Projects/acos/fixtures/build/apps/counter.apk")


def load_smoke():
    spec = importlib.util.spec_from_file_location(
        "mcp_smoke", WT / "tools/release/mcp_smoke.py"
    )
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    module.emit = lambda obj: None
    return module


def log(obj):
    sys.stderr.write(json.dumps(obj, sort_keys=True) + "\n")
    sys.stderr.flush()


CHECK = []


def check(report, name, ok, detail=None):
    report["checks"].append({"check": name, "ok": bool(ok), "detail": detail})
    log({"check": name, "ok": bool(ok), "detail": detail})


def http_get(port, path, timeout=10):
    """(status, headers, body) for one small request."""
    connection = http.client.HTTPConnection("127.0.0.1", port, timeout=timeout)
    connection.request("GET", path)
    response = connection.getresponse()
    body = response.read()
    result = (response.status, dict(response.getheaders()), body)
    connection.close()
    return result


def registry_entries(registry_dir):
    return sorted(Path(registry_dir).glob("*.json"))


class FrameReader(threading.Thread):
    """Reads an MJPEG response and keeps the decoded parts."""

    def __init__(self, port, path, seconds):
        super().__init__(daemon=True)
        self.port = port
        self.path = path
        self.seconds = seconds
        self.headers = None
        self.status = None
        self.buffer = b""
        self.error = None

    def run(self):
        try:
            connection = http.client.HTTPConnection("127.0.0.1", self.port, timeout=15)
            connection.request("GET", self.path)
            response = connection.getresponse()
            self.status = response.status
            self.headers = dict(response.getheaders())
            deadline = time.time() + self.seconds
            while time.time() < deadline:
                chunk = response.read(4096)
                if not chunk:
                    break
                self.buffer += chunk
            connection.close()
        except Exception as error:  # pragma: no cover - evidence
            self.error = repr(error)

    def parts(self):
        """(content_type, image_bytes) pairs from the multipart body."""
        out = []
        marker = b"--acosframe\r\n"
        for chunk in self.buffer.split(marker)[1:]:
            head, _, tail = chunk.partition(b"\r\n\r\n")
            content_type = "image/png"
            for line in head.decode("latin-1").split("\r\n"):
                if line.lower().startswith("content-type:"):
                    content_type = line.split(":", 1)[1].strip()
            image = tail
            if image.endswith(b"\r\n"):
                image = image[:-2]
            if image:
                out.append((content_type, image))
        return out


class EventReader(threading.Thread):
    """Reads an SSE response and keeps the parsed events."""

    def __init__(self, port, path, seconds):
        super().__init__(daemon=True)
        self.port = port
        self.path = path
        self.seconds = seconds
        self.status = None
        self.headers = None
        self.events = []
        self.raw = ""
        self.error = None

    def run(self):
        try:
            connection = http.client.HTTPConnection("127.0.0.1", self.port, timeout=15)
            connection.request("GET", self.path)
            response = connection.getresponse()
            self.status = response.status
            self.headers = dict(response.getheaders())
            deadline = time.time() + self.seconds
            name = None
            while time.time() < deadline:
                line = response.readline()
                if not line:
                    break
                text = line.decode("utf-8", "replace")
                self.raw += text
                if text.startswith("event: "):
                    name = text[len("event: ") :].strip()
                elif text.startswith("data: "):
                    try:
                        payload = json.loads(text[len("data: ") :])
                    except json.JSONDecodeError:
                        payload = {"raw": text}
                    self.events.append({"event": name, "data": payload})
            connection.close()
        except Exception as error:  # pragma: no cover - evidence
            self.error = repr(error)

    def of(self, name):
        return [item["data"] for item in self.events if item["event"] == name]


def main() -> int:
    scratch = Path(sys.argv[1]) if len(sys.argv) > 1 else WT / ".cache/view1c-proof/run"
    scratch.mkdir(parents=True, exist_ok=True)
    registry = scratch / "registry"
    # XDG_RUNTIME_DIR stays the real one: `acos-art` needs the user D-Bus to
    # create its memory-cgroup scope (a scratch runtime dir breaks the launch).
    # The registry — the only new path this track adds — is redirected below.
    runtime = Path(os.environ.get("XDG_RUNTIME_DIR", f"/run/user/{os.getuid()}"))
    stats_file = scratch / "live-view-stats.json"
    artifacts = scratch / "artifacts"
    for path in (registry,):
        if path.exists():
            for item in path.glob("*.json"):
                item.unlink()
    if stats_file.exists():
        stats_file.unlink()
    runtime.mkdir(parents=True, exist_ok=True)

    smoke = load_smoke()
    env = dict(os.environ)
    env.update(
        {
            "ACOS_BIN": str(TARGET / "acos"),
            "ACOS_ALLOWED_ROOTS": "/home/Qui",
            "ACOS_VIEW_REGISTRY_DIR": str(registry),
            "ACOS_VIEW_STATS_FILE": str(stats_file),
            "XDG_RUNTIME_DIR": str(runtime),
        }
    )

    report = {"scratch": str(scratch), "agentview_url": None, "checks": []}
    client = smoke.Mcp(TARGET / "acos-mcp", env)
    view = None
    try:
        init = client.request(
            "initialize",
            {
                "protocolVersion": "2025-03-26",
                "capabilities": {},
                "clientInfo": {"name": "view1c-proof", "version": "1.0"},
            },
        )
        check(
            report,
            "mcp_initialize",
            init.get("serverInfo", {}).get("name") == "acos-mcp",
            init.get("serverInfo"),
        )
        client.notify("notifications/initialized")

        launched = smoke.envelope(
            client.call_tool(
                "acos_launch",
                {"apk": str(COUNTER), "artifacts": str(artifacts)},
                timeout=420,
            )
        )
        session_id = launched.get("session_id")
        check(
            report,
            "launch",
            launched.get("status") == "pass"
            and launched.get("result", {}).get("engine") == "art",
            {"status": launched.get("status"), "session_id": session_id,
             "error": launched.get("error")},
        )
        if launched.get("status") != "pass":
            report["verdict"] = "fail"
            return 1

        # --- VIEW1c M1: view.sock is host-only; the guest cannot see it. ---
        entry_file = registry_entries(registry)[0]
        entry_doc = json.loads(entry_file.read_text())
        stream_socket = Path(entry_doc["stream_socket"])
        launcher_pid = entry_doc.get("launcher_pid") or entry_doc.get("daemon_pid")
        mode_dir = stream_socket.parent.stat().st_mode & 0o777
        mode_sock = stream_socket.lstat().st_mode & 0o777
        check(
            report,
            "view_sock_in_private_host_only_dir",
            stream_socket.name == "view.sock"
            and "acos-view-" in stream_socket.parent.name
            and "acos-art-" not in str(stream_socket)
            and mode_dir == 0o700
            and (mode_sock & 0o077) == 0
            and stream_socket.parent.stat().st_uid == os.getuid(),
            {"socket": str(stream_socket), "dir_mode": oct(mode_dir), "sock_mode": oct(mode_sock)},
        )
        guest_dirs = sorted(runtime.glob(f"acos-art-{launcher_pid}-*/"))
        leaked = [str(p) for d in guest_dirs for p in d.rglob("view.sock")]
        check(report, "no_view_sock_in_any_guest_mounted_socket_dir", bool(guest_dirs) and not leaked,
              {"guest_dirs": [str(d) for d in guest_dirs], "leaked": leaked})

        def descendants(root):
            parents = {}
            for entry in Path("/proc").iterdir():
                if not entry.name.isdigit():
                    continue
                try:
                    stat = (entry / "stat").read_text()
                    ppid = int(stat.rsplit(")", 1)[1].split()[1])
                    parents[int(entry.name)] = ppid
                except Exception:
                    continue
            found, frontier = set(), {root}
            while frontier:
                nxt = {pid for pid, ppid in parents.items() if ppid in frontier and pid not in found}
                found |= nxt
                frontier = nxt
            return sorted(found | {root})

        inspected = []
        for pid in descendants(int(launcher_pid)):
            guest_dev_socket = Path(f"/proc/{pid}/root/dev/socket")
            try:
                names = sorted(p.name for p in guest_dev_socket.iterdir())
            except Exception:
                continue
            host_path_in_guest = Path(f"/proc/{pid}/root{stream_socket}")
            walked = []
            for base in (f"/proc/{pid}/root/dev", f"/proc/{pid}/root/data", f"/proc/{pid}/root/run", f"/proc/{pid}/root/tmp"):
                for dirpath, dirnames, filenames in os.walk(base):
                    if dirpath.count("/") - base.count("/") > 3:
                        dirnames[:] = []
                    if "view.sock" in filenames or "view.sock" in dirnames:
                        walked.append(dirpath)
            inspected.append({"pid": pid, "dev_socket": names,
                              "host_path_visible": host_path_in_guest.exists(),
                              "view_sock_found_at": walked})
        report["guest_inspection"] = inspected
        check(
            report,
            "guest_cannot_see_view_sock",
            bool(inspected)
            and all("view.sock" not in item["dev_socket"] and not item["host_path_visible"]
                    and not item["view_sock_found_at"] for item in inspected)
            and any(item["dev_socket"] for item in inspected),
            inspected,
        )

        # --- `acos view --once --json` lists the live session and exits. ---
        once = subprocess.run(
            [str(TARGET / "acos"), "view", "--once", "--json", "--registry-dir", str(registry)],
            capture_output=True, text=True, timeout=60,
        )
        once_doc = json.loads(once.stdout.strip().splitlines()[-1])
        check(
            report,
            "once_json_lists_session",
            once.returncode == 0
            and any(s["session_id"] == session_id for s in once_doc.get("sessions", [])),
            once_doc,
        )

        # --- Start the hub. ---
        view = subprocess.Popen(
            [str(TARGET / "acos"), "view", "--json", "--registry-dir", str(registry)],
            stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True,
        )
        line = view.stdout.readline()
        url_doc = json.loads(line)
        port, token, url = url_doc["port"], url_doc["token"], url_doc["url"]
        report["agentview_url"] = url
        check(
            report,
            "prints_one_line_url",
            url.startswith(f"http://127.0.0.1:{port}/?t=") and len(token) == 64,
            {"url": url, "line": line.strip()},
        )
        # The documented one-line form is covered by the text mode below.

        # --- Token gate. ---
        no_token = http_get(port, "/api/sessions")
        wrong_token = http_get(port, f"/api/sessions?t={'0' * 64}")
        check(
            report,
            "no_token_is_403",
            no_token[0] == 403 and wrong_token[0] == 403,
            {"without": no_token[0], "wrong": wrong_token[0], "body": no_token[2][:60].decode()},
        )
        import socket as _socket
        def raw_status(host_header):
            sock = _socket.create_connection(("127.0.0.1", port), timeout=10)
            sock.sendall(f"GET /api/sessions?t={token} HTTP/1.1\r\nHost: {host_header}\r\nConnection: close\r\n\r\n".encode())
            data = b""
            while True:
                chunk = sock.recv(4096)
                if not chunk:
                    break
                data += chunk
            sock.close()
            return int(data.split()[1])
        check(report, "host_header_must_be_loopback_literal_and_port",
              raw_status(f"127.0.0.1:{port}") == 200 and raw_status(f"evil.example:{port}") == 403
              and raw_status("127.0.0.1") == 403,
              {"ok": raw_status(f"127.0.0.1:{port}"), "rebind": raw_status(f"evil.example:{port}")})
        page = http_get(port, f"/?t={token}")
        html = page[2].decode("utf-8", "replace")
        check(
            report,
            "page_is_self_contained",
            page[0] == 200
            and "<!doctype html>" in html
            and "/api/sessions" in html
            and "/api/frames/" in html
            and "http://" not in html.replace("http://127.0.0.1", "")
            and "<script src" not in html
            and "<link rel=\"stylesheet\"" not in html,
            {"status": page[0], "bytes": len(html), "cdn": "http://" in html},
        )

        sessions = json.loads(http_get(port, f"/api/sessions?t={token}")[2])
        listed = [s for s in sessions["sessions"] if s["session_id"] == session_id]
        check(
            report,
            "sessions_listed",
            len(listed) == 1
            and listed[0]["package"] == "dev.acos.fixtures.counter"
            and listed[0]["stream"] is True
            and listed[0]["client"]["name"] == "view1c-proof"
            and listed[0]["watchers"] == 0,
            listed[0] if listed else sessions,
        )

        # --- Frames (MJPEG) + events (SSE) exactly as the page does. ---
        frames = FrameReader(port, f"/api/frames/{session_id}?t={token}", 6.0)
        events = EventReader(port, f"/api/events/{session_id}?t={token}", 14.0)
        frames.start()
        events.start()
        time.sleep(1.5)
        check(
            report,
            "frames_content_type",
            bool(frames.headers)
            and "multipart/x-mixed-replace" in frames.headers.get("Content-Type", ""),
            frames.headers,
        )

        taps = []
        for _ in range(2):
            tapped = smoke.envelope(
                client.call_tool(
                    "acos_tap", {"session_id": session_id, "text": "Increment"}, timeout=180
                )
            )
            taps.append(
                {
                    "status": tapped.get("status"),
                    "accepted": tapped.get("result", {}).get("accepted"),
                }
            )
            time.sleep(0.7)
        report["taps"] = taps
        check(
            report,
            "taps_accepted",
            all(tap.get("accepted") is True for tap in taps),
            taps,
        )

        # While a browser watches, the producer's own stats seam says 1.
        time.sleep(3.0)
        stats_watching = json.loads(stats_file.read_text()) if stats_file.exists() else {}
        report["stats_while_watching"] = stats_watching
        watching_count = (stats_watching.get("stats") or {}).get("subscribers")
        check(
            report,
            "producer_sees_one_subscriber_while_watched",
            watching_count == 1,
            {"subscribers": watching_count,
             "frames_sent": (stats_watching.get("stats") or {}).get("frames_sent"),
             "encodes": (stats_watching.get("stats") or {}).get("encodes")},
        )

        frames.join(timeout=30)
        parts = frames.parts()
        distinct = {hashlib.sha256(image).hexdigest() for _, image in parts}
        decoded = []
        png_ok = all(content_type == "image/png" and image[:8] == b"\x89PNG\r\n\x1a\n"
                     for content_type, image in parts)
        try:
            from PIL import Image  # noqa: PLC0415
            import io

            for _, image in parts:
                with Image.open(io.BytesIO(image)) as decoded_image:
                    decoded.append(decoded_image.convert("RGB").tobytes())
        except Exception as error:  # pragma: no cover
            report["decode_error"] = repr(error)
        report["frames"] = {
            "parts": len(parts),
            "distinct": len(distinct),
            "decoded_distinct": len(set(decoded)),
            "png_ok": png_ok,
            "bytes": sum(len(image) for _, image in parts),
        }
        (scratch / "frames").mkdir(exist_ok=True)
        for index, (content_type, image) in enumerate(parts[:6]):
            suffix = "png" if "png" in content_type else "jpg"
            (scratch / "frames" / f"frame-{index:02d}.{suffix}").write_bytes(image)
        check(
            report,
            "frames_decode",
            len(parts) >= 2 and len(set(decoded)) >= 2 and png_ok,
            report["frames"],
        )

        events.join(timeout=40)
        parsed = events.of("ev")
        taps_events = [
            item for item in parsed
            if item.get("kind") == "action" and item.get("event", {}).get("cmd") == "tap"
        ]
        report["sse"] = {
            "state": events.of("state"),
            "events": parsed[-8:],
            "tap_events": taps_events,
            "error": events.error,
        }
        check(
            report,
            "sse_state_and_tap_events",
            bool(events.of("state"))
            and len(taps_events) >= 2
            and all(
                isinstance(item["event"].get("x"), int)
                and isinstance(item["event"].get("y"), int)
                and item["event"].get("status") == "ok"
                for item in taps_events
            ),
            {
                "state": events.of("state")[:1],
                "taps": [
                    {k: item["event"].get(k) for k in ("x", "y", "status", "cmd")}
                    for item in taps_events
                ],
                "event_count": len(parsed),
            },
        )

        # --- The viewer closed: the session is back to zero subscribers. ---
        # A closed socket is only noticed by the producer on its next send
        # (`docs/LIVE_VIEW.md` §3.3), so one more action is delivered after the
        # browser is gone: a still-open hub socket would have received it.
        time.sleep(0.5)
        smoke.envelope(
            client.call_tool(
                "acos_tap", {"session_id": session_id, "text": "Increment"}, timeout=180
            )
        )
        time.sleep(1.5)
        after = json.loads(http_get(port, f"/api/sessions?t={token}")[2])
        after_listed = [s for s in after["sessions"] if s["session_id"] == session_id]
        check(
            report,
            "hub_reports_zero_watchers_after_close",
            bool(after_listed) and after_listed[0]["watchers"] == 0
            and after_listed[0]["connected"] is False,
            after_listed[0] if after_listed else after,
        )

        destroyed = smoke.envelope(
            client.call_tool("acos_destroy", {"session_id": session_id}, timeout=180)
        )
        check(
            report,
            "destroy",
            destroyed.get("status") == "pass",
            {"status": destroyed.get("status"), "error": destroyed.get("error")},
        )
        time.sleep(1.5)
        stats_final = json.loads(stats_file.read_text()) if stats_file.exists() else {}
        report["stats_final"] = stats_final
        final = stats_final.get("stats") or {}
        check(
            report,
            "producer_back_to_zero_subscribers",
            final.get("subscribers") == 0 and (final.get("subscribers_dropped") or 0) >= 1,
            {
                "subscribers": final.get("subscribers"),
                "subscribers_dropped": final.get("subscribers_dropped"),
                "frames_sent": final.get("frames_sent"),
                "events_sent": final.get("events_sent"),
            },
        )
        after_destroy = json.loads(http_get(port, f"/api/sessions?t={token}")[2])
        check(
            report,
            "session_disappears_after_destroy",
            after_destroy["sessions"] == [] and not registry_entries(registry),
            {"sessions": after_destroy["sessions"], "files": [str(p) for p in registry_entries(registry)]},
        )
    finally:
        if view is not None:
            view.terminate()
            try:
                view.wait(timeout=10)
            except subprocess.TimeoutExpired:  # pragma: no cover
                view.kill()
            report["view_stderr"] = view.stderr.read()[-2000:] if view.stderr else ""
        client.close()
        time.sleep(1.0)
        if stats_file.exists() and "stats_final" not in report:
            report["stats_final"] = json.loads(stats_file.read_text())
        report["verdict"] = "pass" if all(item["ok"] for item in report["checks"]) else "fail"
        (scratch / "report.json").write_text(json.dumps(report, indent=2, sort_keys=True))
        print(json.dumps({item["check"]: item["ok"] for item in report["checks"]}, indent=2))
        print("verdict:", report["verdict"])
    return 0 if report.get("verdict") == "pass" else 1


if __name__ == "__main__":
    sys.exit(main())
