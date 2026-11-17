"""SPIKE-WV host broker: one Chromium, one browser context per session.

This is the host side of ADR-0031 item 1. It owns the DevTools pipe to a pinned
`chrome-headless-shell` and exposes a small, framed request/response protocol on
a Unix socket that ACOS creates for the guest:

    {"id": 7, "op": "navigate", "session": "s1", "url": "https://example.com/"}
    {"id": 7, "ok": true, "result": {...}}
    {"id": 8, "ok": false, "error": {"code": "NAVIGATION_FAILED", "message": "..."}}

The guest never sees a DevTools socket, port, target id or session id: `session`
is the broker's own opaque handle. Nothing here paints; the Java provider pulls
PNG frames through `frame` and writes them into the View canvas.

Run the gate measurements:  python3 wv_broker.py --selfcheck --out evidence/
Serve one broker:           python3 wv_broker.py --serve --socket /path/to.sock
"""

from __future__ import annotations

import argparse
import base64
import json
import os
import socket
import subprocess
import sys
import threading
import time
from collections import OrderedDict
from typing import Any

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from cdp import CDP, CDPError  # noqa: E402

HERE = os.path.dirname(os.path.abspath(__file__))
DEFAULT_SHELL = os.environ.get(
    "ACOS_CHROME_HEADLESS_SHELL",
    os.path.join(HERE, "..", "..", "..", ".cache", "spike-wv", "cft",
                 "chrome-headless-shell-linux64", "chrome-headless-shell"),
)

# Flags: no profile reuse (a fresh throwaway user-data-dir), no first-run
# traffic, deterministic frame control, no GPU (the host has none for us).
BASE_FLAGS = [
    "--headless",
    "--enable-begin-frame-control",
    # deterministic frame production (HeadlessExperimental.beginFrame requires it)
    "--run-all-compositor-stages-before-draw",
    "--disable-new-content-rendering-timeout",
    "--disable-gpu",
    "--no-first-run",
    "--no-default-browser-check",
    "--disable-background-networking",
    "--disable-background-timer-throttling",
    "--disable-breakpad",
    "--disable-component-update",
    "--disable-domain-reliability",
    "--disable-extensions",
    "--disable-sync",
    "--metrics-recording-only",
    "--mute-audio",
    # touch input reaches the page (the provider sends DevTools touch, not mouse)
    "--touch-events=enabled",
    "--no-pings",
    "--font-render-hinting=none",
]


def page_png_data(data: str) -> bytes:
    return base64.b64decode(data)


class Page:
    """One guest session = one browser context + one target + one CDP session."""

    def __init__(self, browser: "Browser", context_id: str, target_id: str, session_id: str):
        self.browser = browser
        self.context_id = context_id
        self.target_id = target_id
        self.session_id = session_id
        self.cdp = browser.cdp
        self.events = self.cdp.subscribe(session_id)
        self.bindings: dict[str, list[str]] = {}
        self.cdp.call("Page.enable", session_id=self.session_id)
        self.cdp.call("Runtime.enable", session_id=self.session_id)
        self.cdp.call("DOM.enable", session_id=self.session_id)
        self.cdp.call("Accessibility.enable", session_id=self.session_id)
        self.viewport = {"width": 1080, "height": 800, "dpr": 2.625}

    # --- viewport / rendering ---------------------------------------------
    def set_view(self, width: int, height: int, dpr: float) -> None:
        self.viewport = {"width": width, "height": height, "dpr": dpr}
        self.cdp.call("Emulation.setDeviceMetricsOverride", {
            "width": width, "height": height, "deviceScaleFactor": dpr, "mobile": True,
        }, session_id=self.session_id)

    def frame(self, timeout: float = 5.0) -> dict[str, Any]:
        """Deterministic frame: HeadlessExperimental.beginFrame + PNG screenshot."""
        t0 = time.perf_counter()
        res = self.cdp.call("HeadlessExperimental.beginFrame", {
            "screenshot": {"format": "png", "optimizeForSpeed": True},
        }, session_id=self.session_id, timeout=timeout)
        ms = (time.perf_counter() - t0) * 1000.0
        png = base64.b64decode(res["screenshotData"]) if res.get("screenshotData") else b""
        return {"png": png, "begin_frame_ms": ms,
                "has_damage": res.get("hasDamage", True),
                "dom_visual_viewport": res.get("mainFrameContentSize", {})}

    def screencast_frame(self, timeout: float = 5.0) -> dict[str, Any]:
        """Fallback frame source: Page.startScreencast (JPEG)."""
        q = self.events
        self.cdp.call("Page.startScreencast", {"format": "jpeg", "quality": 80,
                                               "maxWidth": self.viewport["width"],
                                               "maxHeight": self.viewport["height"]},
                      session_id=self.session_id)
        try:
            ev = self.cdp.wait_event(q, "Page.screencastFrame", timeout=timeout)
        finally:
            self.cdp.call("Page.stopScreencast", session_id=self.session_id)
        self.cdp.call("Page.screencastFrameAck", {"sessionId": ev["sessionId"]},
                      session_id=self.session_id)
        return {"jpeg": base64.b64decode(ev["data"]), "metadata": ev.get("metadata", {})}

    # --- navigation --------------------------------------------------------
    def navigate(self, url: str, timeout: float = 30.0) -> dict[str, Any]:
        t0 = time.perf_counter()
        res = self.cdp.call("Page.navigate", {"url": url}, session_id=self.session_id,
                            timeout=timeout)
        if res.get("errorText"):
            raise CDPError(f"navigate failed: {res['errorText']}")
        # Wait for the load event on the frame we asked for.
        self.cdp.wait_event(self.events, "Page.loadEventFired", timeout=timeout)
        ms = (time.perf_counter() - t0) * 1000.0
        doc = self.eval("({url: location.href, title: document.title, "
                        "ready: document.readyState, nodes: document.getElementsByTagName('*').length})")
        doc["load_ms"] = ms
        return doc

    # --- javascript --------------------------------------------------------
    def eval(self, expression: str, timeout: float = 30.0) -> Any:
        res = self.cdp.call("Runtime.evaluate", {
            "expression": expression, "returnByValue": True, "awaitPromise": True,
            "userGesture": True,
        }, session_id=self.session_id, timeout=timeout)
        if "exceptionDetails" in res:
            raise CDPError("js exception: " + json.dumps(res["exceptionDetails"])[:500])
        return res.get("result", {}).get("value")

    def add_binding(self, name: str) -> None:
        self.bindings.setdefault(name, [])
        self.cdp.call("Runtime.addBinding", {"name": name}, session_id=self.session_id)

    def wait_binding(self, name: str, timeout: float = 10.0) -> list[str]:
        """Page -> host direction: Runtime.bindingCalled payloads (JSON, in order)."""
        out: list[str] = []
        deadline = time.monotonic() + timeout
        while not out:
            left = max(0.0, deadline - time.monotonic())
            if left == 0:
                raise TimeoutError(f"binding {name} never called")
            ev = self.cdp.wait_event(
                self.events, "Runtime.bindingCalled",
                lambda p: p.get("name") == name, timeout=left)
            out = list(self.bindings[name]) + [ev["payload"]]
            self.bindings[name].append(ev["payload"])
        return out

    # --- accessibility tree -------------------------------------------------
    def ax_nodes(self) -> list[dict[str, Any]]:
        tree = self.cdp.call("Accessibility.getFullAXTree", {}, session_id=self.session_id,
                             timeout=30.0)
        out = []
        for n in tree.get("nodes", []):
            if n.get("ignored"):
                continue
            role = (n.get("role") or {}).get("value")
            name = (n.get("name") or {}).get("value")
            out.append({
                "ax_id": n.get("nodeId"),
                "role": role,
                "name": name,
                "backend_dom_node_id": n.get("backendDOMNodeId"),
                "value": (n.get("value") or {}).get("value"),
            })
        return out

    def find_by_text(self, text: str, roles: tuple[str, ...] | None = None) -> dict[str, Any] | None:
        needle = text.strip().lower()
        for n in self.ax_nodes():
            if roles and n["role"] not in roles:
                continue
            name = (n["name"] or "").strip().lower()
            if name == needle or needle in name:
                return n
        return None

    def box(self, backend_dom_node_id: int) -> dict[str, float]:
        """Content-box bounds in CSS pixels, from DOM.getBoxModel."""
        m = self.cdp.call("DOM.getBoxModel", {"backendNodeId": backend_dom_node_id},
                          session_id=self.session_id)
        model = m["model"]
        quad = model["content"]
        xs = quad[0::2]
        ys = quad[1::2]
        return {"x": min(xs), "y": min(ys), "w": max(xs) - min(xs), "h": max(ys) - min(ys)}

    # --- input -------------------------------------------------------------
    def tap(self, x: float, y: float, hold_ms: int = 40) -> dict[str, Any]:
        """Touch down/up at a CSS-pixel point, driven through the frame pipeline.

        With `--enable-begin-frame-control` the touch points are only routed when
        a compositor frame is produced, and the DevTools replies for touch input
        arrive *after* that frame. So: send, produce a frame, send, produce a
        frame, then drain the (informational) acks without blocking the caller.
        """
        if not getattr(self, "_touch_on", False):
            self.cdp.call("Emulation.setTouchEmulationEnabled",
                          {"enabled": True, "maxTouchPoints": 5},
                          session_id=self.session_id)
            self._touch_on = True
        down = self.cdp.send_raw("Input.dispatchTouchEvent", {
            "type": "touchStart",
            "touchPoints": [{"x": x, "y": y, "radiusX": 4, "radiusY": 4,
                             "force": 1, "id": 1}],
        }, session_id=self.session_id)
        first = self.frame()
        time.sleep(hold_ms / 1000.0)
        up = self.cdp.send_raw("Input.dispatchTouchEvent", {"type": "touchEnd",
                                                            "touchPoints": []},
                               session_id=self.session_id)
        second = self.frame()
        acks = {}
        for name, mid in (("down", down), ("up", up)):
            try:
                self.cdp.result(mid, timeout=2.0)
                acks[name] = "acked"
            except (CDPError, TimeoutError):
                acks[name] = "no-ack"
        return {"acks": acks, "frame_ms": [round(first["begin_frame_ms"], 2),
                                           round(second["begin_frame_ms"], 2)]}

    def tap_text(self, text: str) -> dict[str, Any]:
        node = self.find_by_text(text)
        if node is None:
            return {"found": False, "text": text}
        b = self.box(node["backend_dom_node_id"])
        t = self.tap(b["x"] + b["w"] / 2.0, b["y"] + b["h"] / 2.0)
        return {"found": True, "text": text, "role": node["role"], "bounds": b,
                "role_path": node, "input": t}

    def scroll(self, dx: float, dy: float) -> None:
        self.cdp.call("Input.dispatchMouseEvent", {
            "type": "mouseWheel", "x": 100, "y": 100, "deltaX": dx, "deltaY": dy,
        }, session_id=self.session_id)

    # --- teardown ----------------------------------------------------------
    def close(self) -> None:
        self.cdp.unsubscribe(self.events)
        try:
            self.cdp.call("Target.closeTarget", {"targetId": self.target_id}, timeout=10)
        except CDPError:
            pass
        try:
            self.cdp.call("Target.disposeBrowserContext", {"browserContextId": self.context_id},
                          timeout=10)
        except CDPError:
            pass


class Browser:
    """One Chromium process for the whole host; contexts per session."""

    def __init__(self, shell: str = DEFAULT_SHELL, user_data_dir: str | None = None,
                 extra_flags: tuple[str, ...] = ()):
        self.shell = shell
        self.user_data_dir = user_data_dir or os.path.join(
            os.environ.get("ACOS_SPIKE_WV_SCRATCH", "/tmp"), "spike-wv-profile-%d" % os.getpid())
        os.makedirs(self.user_data_dir, exist_ok=True)
        argv = [shell, *BASE_FLAGS, *extra_flags,
                "--user-data-dir=" + self.user_data_dir,
                "--remote-debugging-pipe"]
        self.argv = argv
        self.cdp = CDP(argv)
        self.pages: "OrderedDict[str, Page]" = OrderedDict()

    # --- browser level -----------------------------------------------------
    def version(self) -> dict[str, Any]:
        return self.cdp.call("Browser.getVersion")

    def new_page(self, width: int = 1080, height: int = 800, dpr: float = 2.625,
                 url: str = "about:blank") -> Page:
        ctx = self.cdp.call("Target.createBrowserContext",
                            {"disposeOnDetach": False})["browserContextId"]
        tgt = self.cdp.call("Target.createTarget",
                            {"url": url, "browserContextId": ctx})
        sess = self.cdp.call("Target.attachToTarget",
                             {"targetId": tgt["targetId"], "flatten": True})["sessionId"]
        page = Page(self, ctx, tgt["targetId"], sess)
        page.set_view(width, height, dpr)
        self.pages[sess] = page
        return page

    def process_tree(self) -> dict[str, Any]:
        """RSS of the broker's Chromium tree (MiB), by process."""
        pids = [self.cdp.proc.pid]
        children: dict[int, list[int]] = {}
        for entry in os.listdir("/proc"):
            if not entry.isdigit():
                continue
            pid = int(entry)
            try:
                with open(f"/proc/{pid}/stat", "rb") as fh:
                    stat = fh.read()
                ppid = int(stat.rsplit(b")", 1)[1].split()[1])
            except (OSError, IndexError, ValueError):
                continue
            children.setdefault(ppid, []).append(pid)
        seen = []
        stack = list(pids)
        while stack:
            pid = stack.pop()
            if pid in seen:
                continue
            seen.append(pid)
            stack.extend(children.get(pid, []))
        procs = []
        total = 0
        total_pss = 0.0
        for pid in seen:
            try:
                with open(f"/proc/{pid}/statm", "rb") as fh:
                    rss_pages = int(fh.read().split()[1])
                with open(f"/proc/{pid}/cmdline", "rb") as fh:
                    cmd = fh.read().replace(b"\0", b" ").decode(errors="replace")
            except OSError:
                continue
            mib = rss_pages * os.sysconf("SC_PAGE_SIZE") / 1048576.0
            pss = 0.0
            try:
                with open(f"/proc/{pid}/smaps_rollup") as fh:
                    for line in fh:
                        if line.startswith("Pss:"):
                            pss = int(line.split()[1]) / 1024.0
                            break
            except OSError:
                pss = mib
            total += mib
            total_pss += pss
            procs.append({"pid": pid, "rss_mib": round(mib, 1),
                          "pss_mib": round(pss, 1), "cmd": cmd[:120]})
        return {"total_rss_mib": round(total, 1), "total_pss_mib": round(total_pss, 1),
                "processes": procs}

    def close(self) -> None:
        for p in list(self.pages.values()):
            p.close()
        self.cdp.close()


# ---------------------------------------------------------------------------
# Broker protocol (what the guest is allowed to see)
# ---------------------------------------------------------------------------

class Broker:
    def __init__(self, shell: str | None = None):
        self.browser = Browser(shell or DEFAULT_SHELL)
        self.sessions: dict[str, Page] = {}
        self._n = 0
        self.lock = threading.Lock()

    def handle(self, req: dict[str, Any]) -> dict[str, Any]:
        op = req.get("op")
        sid = req.get("session")
        try:
            if op == "ping":
                v = self.browser.version()
                return {"engine": v.get("product"), "protocol": v.get("protocolVersion")}
            if op == "engine":
                v = self.browser.version()
                return {"product": v.get("product"), "revision": v.get("revision"),
                        "user_agent": v.get("userAgent"), "argv": self.browser.argv[1:]}
            if op == "open":
                if sid and sid in self.sessions:
                    raise CDPError("session exists")
                page = self.browser.new_page(
                    int(req.get("width", 1080)), int(req.get("height", 800)),
                    float(req.get("dpr", 2.625)))
                with self.lock:
                    self._n += 1
                    name = sid or f"wv{self._n}"
                self.sessions[name] = page
                return {"session": name, "viewport": page.viewport}
            if op == "close":
                page = self.session(sid)
                page.close()
                self.sessions.pop(sid, None)
                return {"closed": sid}
            if op == "navigate":
                return self.session(sid).navigate(req["url"])
            if op == "eval":
                return {"value": self.session(sid).eval(req["expression"])}
            if op == "bind":
                self.session(sid).add_binding(req["name"])
                return {"binding": req["name"]}
            if op == "binding_poll":
                return {"payloads": self.session(sid).wait_binding(
                    req["name"], float(req.get("timeout_s", 10)))}
            if op == "frame":
                f = self.session(sid).frame(float(req.get("timeout_s", 5)))
                return {"begin_frame_ms": f["begin_frame_ms"],
                        "png_b64": base64.b64encode(f["png"]).decode() if req.get("png") else None}
            if op == "frame_stats":
                page = self.session(sid)
                times = []
                for _ in range(int(req.get("n", 10))):
                    f = page.frame()
                    times.append(f["begin_frame_ms"])
                times.sort()
                return {"n": len(times), "median_ms": times[len(times) // 2],
                        "p95_ms": times[max(0, int(len(times) * 0.95) - 1)],
                        "min_ms": times[0], "max_ms": times[-1]}
            if op == "ax":
                return {"nodes": self.session(sid).ax_nodes()}
            if op == "tap_text":
                return self.session(sid).tap_text(req["text"])
            if op == "tap_xy":
                self.session(sid).tap(float(req["x"]), float(req["y"]))
                return {"tapped": [req["x"], req["y"]]}
            if op == "scroll":
                self.session(sid).scroll(float(req.get("dx", 0)), float(req.get("dy", 0)))
                return {"ok": True}
            if op == "memory":
                return self.browser.process_tree()
            raise CDPError(f"unknown op {op!r}")
        except (CDPError, TimeoutError, KeyError) as exc:
            return {"ok": False, "error": {"code": type(exc).__name__.upper(),
                                           "message": str(exc)[:2000]}}

    def session(self, sid: str) -> Page:
        page = self.sessions.get(sid or "")
        if page is None:
            raise CDPError(f"no such session {sid!r}")
        return page

    def serve(self, path: str) -> None:
        try:
            os.unlink(path)
        except FileNotFoundError:
            pass
        srv = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        srv.bind(path)
        os.chmod(path, 0o600)
        srv.listen(8)
        sys.stderr.write("spike-wv broker listening on %s (engine %s)\n"
                         % (path, self.browser.version().get("product")))
        sys.stderr.flush()
        while True:
            conn, _ = srv.accept()
            threading.Thread(target=self._serve_conn, args=(conn,), daemon=True).start()

    def _serve_conn(self, conn: socket.socket) -> None:
        buf = b""
        with conn:
            while True:
                try:
                    chunk = conn.recv(1 << 16)
                except OSError:
                    return
                if not chunk:
                    return
                buf += chunk
                while b"\n" in buf:
                    line, buf = buf.split(b"\n", 1)
                    if not line.strip():
                        continue
                    try:
                        req = json.loads(line)
                    except json.JSONDecodeError as exc:
                        conn.sendall(json.dumps({"ok": False, "error": {
                            "code": "BAD_REQUEST", "message": str(exc)}}).encode() + b"\n")
                        continue
                    out = self.handle(req)
                    out.setdefault("ok", True)
                    out["id"] = req.get("id")
                    conn.sendall(json.dumps(out).encode() + b"\n")


def main(argv: list[str]) -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--serve", metavar="SOCKET")
    ap.add_argument("--selfcheck", action="store_true")
    ap.add_argument("--out", default=os.path.join(HERE, "..", "evidence"))
    ap.add_argument("--shell", default=DEFAULT_SHELL)
    args = ap.parse_args(argv)
    if args.serve:
        Broker(args.shell).serve(args.serve)
        return 0
    if args.selfcheck:
        import measure
        return measure.run(args.out, args.shell)
    ap.print_help()
    return 2


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
