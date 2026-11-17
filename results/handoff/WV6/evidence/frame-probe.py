#!/usr/bin/env python3
"""WV6 host-side WIRE v1.1 probe: real engine, real page, real frames.

Drives `acos-webview --session` with the normative wire framing, serves the
probe's asset page over `req.intercept` (the same path the guest provider uses),
captures the engine PNG before and after a tap, and checks the `ax` tree text.
This writes the page frames as evidence; it is a test instrument, not a broker.
"""
import base64, json, os, shutil, socket, struct, subprocess, sys, time

MAX_FRAME = 64 * 1024 * 1024
WT = os.environ.get("WT", os.getcwd())
OUT = os.path.join(WT, "results/handoff/WV6/evidence")
SERVER = os.path.join(WT, ".cache/wv6-target/release/acos-webview")
SESSION = os.path.join(WT, ".cache/wv6-run/frame-probe")
ASSET = os.path.join(WT, "fixtures/apps/webview-probe/assets/test.html")
CACHE = "/home/Qui/Projects/acos/.cache/webview"


def recv_exact(sock, n):
    buf = b""
    while len(buf) < n:
        chunk = sock.recv(n - len(buf))
        if not chunk:
            raise EOFError("broker closed")
        buf += chunk
    return buf


def recv_frame(sock):
    (length,) = struct.unpack(">I", recv_exact(sock, 4))
    if not 1 <= length <= MAX_FRAME:
        raise RuntimeError("bad frame length %d" % length)
    kind = recv_exact(sock, 1)[0]
    body = recv_exact(sock, length - 1)
    if kind == 1:
        return json.loads(body.decode("utf-8")), None
    if kind == 2:
        (nid,) = struct.unpack(">I", body[:4])
        p = 4 + nid
        fmt = body[p]
        width, height, stride = struct.unpack(">III", body[p + 1:p + 13])
        (seq,) = struct.unpack(">Q", body[p + 13:p + 21])
        return None, {"format": fmt, "width": width, "height": height,
                      "stride": stride, "seq": seq, "data": body[p + 21:]}
    raise RuntimeError("unknown frame kind %d" % kind)


class Broker:
    def __init__(self, sock):
        self.sock = sock
        self.next_id = 0
        self.events = []
        self.jsi = []

    def send(self, msg):
        payload = json.dumps(msg).encode("utf-8")
        self.sock.sendall(struct.pack(">I", len(payload) + 1) + b"\x01" + payload)

    def answer(self, req, result=None, error=None):
        msg = {"id": req["id"], "ok": error is None}
        if error is None:
            msg["result"] = result or {}
        else:
            msg["error"] = error
        self.send(msg)

    def pump(self, timeout=30.0):
        self.sock.settimeout(timeout)
        msg, blob = recv_frame(self.sock)
        if msg is not None and "op" in msg:
            op = msg["op"]
            if op == "req.intercept":
                url = msg.get("url", "")
                path = url.split("file:///android_asset/", 1)[-1]
                try:
                    data = open(ASSET, "rb").read() if path == "test.html" else b""
                except OSError:
                    data = b""
                if not data:
                    self.answer(msg, {"action": "fail", "reason": "net::ERR_FILE_NOT_FOUND"})
                else:
                    self.answer(msg, {"action": "fulfill", "status": 200,
                                      "mime": "text/html",
                                      "headers": {"access-control-allow-origin": "*"},
                                      "bodyB64": base64.b64encode(data).decode()})
                return None, None
            if op == "nav.shouldOverride":
                self.answer(msg, {"allow": True})
                return None, None
            if op == "js.call":
                self.jsi.append(msg)
                self.answer(msg, {"resultJson": json.dumps("ok")})
                return None, None
            self.answer(msg, error={"code": "UNSUPPORTED", "message": op})
            return None, None
        if msg is not None and "event" in msg:
            self.events.append(msg)
        return msg, blob

    def request(self, op, timeout=40.0, **params):
        self.next_id += 1
        rid = "g%d" % self.next_id
        params.update({"id": rid, "op": op})
        self.send(params)
        while True:
            msg, blob = self.pump(timeout)
            if msg is None:
                if blob is not None and blob.get("for") == rid:
                    return {"blob": blob}
                continue
            if msg.get("id") == rid:
                if not msg.get("ok"):
                    raise RuntimeError("%s: %s" % (op, msg.get("error")))
                out = msg.get("result", {})
                if op == "render":
                    # The BLOB follows its JSON response (WIRE.md §1).
                    msg2, blob2 = self.pump(timeout)
                    if blob2 is not None:
                        out = dict(out)
                        out["blob"] = blob2
                        return out
                    return out
                return out

    def wait_event(self, name, timeout=40.0):
        for i, ev in enumerate(self.events):
            if ev.get("event") == name:
                return self.events.pop(i)
        deadline = time.time() + timeout
        while time.time() < deadline:
            self.pump(min(5.0, deadline - time.time()))
            for i, ev in enumerate(self.events):
                if ev.get("event") == name:
                    return self.events.pop(i)
        raise RuntimeError("no %s event" % name)


def pss_kib(pid):
    try:
        with open("/proc/%d/smaps_rollup" % pid) as f:
            for line in f:
                if line.startswith("Pss:"):
                    return int(line.split()[1])
    except OSError:
        return 0
    return 0


def descendants(root):
    children = {}
    for name in os.listdir("/proc"):
        if not name.isdigit():
            continue
        pid = int(name)
        try:
            stat = open("/proc/%d/stat" % pid).read()
            ppid = int(stat.rsplit(")", 1)[1].split()[1])
        except (OSError, ValueError, IndexError):
            continue
        children.setdefault(ppid, []).append(pid)
    out, queue = [], [root]
    while queue:
        pid = queue.pop()
        out.append(pid)
        queue.extend(children.get(pid, []))
    return out


def find(nodes, text):
    for n in nodes:
        if (n.get("name") or "") == text and n.get("has_bounds"):
            x, y, w, h = n["bounds"]
            return n, x + w / 2.0, y + h / 2.0
    return None, None, None


def main():
    os.makedirs(OUT, exist_ok=True)
    shutil.rmtree(SESSION, ignore_errors=True)
    os.makedirs(SESSION, exist_ok=True)
    env = dict(os.environ)
    env["ACOS_WEBVIEW_CACHE"] = CACHE
    proc = subprocess.Popen([SERVER, "--session", SESSION, "--cache", CACHE],
                            env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    sock_path = os.path.join(SESSION, "webview.sock")
    for _ in range(400):
        if os.path.exists(sock_path):
            break
        time.sleep(0.05)
    session = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    session.connect(sock_path)
    b = Broker(session)
    report = {"engine": None, "frames": {}, "ax": {}, "jsi_calls": [], "events": [],
              "cost": {}}
    try:
        hello = b.request("hello", proto=1.1, client="acos-wv6-frame-probe/1")
        report["engine"] = hello.get("engine")
        vp = {"width": 900, "height": 560, "density": 2.625}
        b.request("view.create", view=1, javascriptEnabled=True, **vp)
        b.request("view.addJavascriptInterface", view=1, name="acosBridge", methods=["report"])
        b.request("view.settings", view=1, settings={"cacheMode": 2, "textZoom": 120})
        t_load = time.monotonic()
        b.request("view.loadUrl", view=1, url="file:///android_asset/test.html")
        b.wait_event("navigation.finished")

        f1 = b.request("render", view=1, format="png")
        report["cost"]["first_frame_ms"] = round((time.monotonic() - t_load) * 1000.0, 1)
        open(os.path.join(OUT, "engine-first-frame.png"), "wb").write(f1["blob"]["data"])
        report["frames"]["first"] = {k: f1[k] for k in ("seq", "width", "height", "engineMs")}
        report["frames"]["first"]["png_bytes"] = len(f1["blob"]["data"])
        report["frames"]["first"]["png_dims"] = [f1["blob"]["width"], f1["blob"]["height"]]

        ax1 = b.request("ax", view=1)
        report["ax"]["before"] = [{"role": n["role"], "name": n.get("name"),
                                   "bounds": n["bounds"], "clickable": n["clickable"]}
                                  for n in ax1["nodes"] if n.get("name")]
        node, x, y = find(ax1["nodes"], "Tap me")
        if node is None:
            raise RuntimeError("ax has no 'Tap me' node")
        report["tap"] = {"x": x, "y": y}
        t_tap = time.monotonic()
        b.request("input.tap", view=1, x=x, y=y)
        report["cost"]["tap_ms"] = round((time.monotonic() - t_tap) * 1000.0, 1)
        # The page's click handler changed #out and called the bridge.
        ax2 = b.request("ax", view=1)
        names = [n.get("name") for n in ax2["nodes"] if n.get("name")]
        report["ax"]["after"] = names
        report["js_out"] = b.request("view.evaluateJavascript", view=1,
                                     script="document.getElementById('out').textContent")
        report["jsi_calls"] = b.jsi
        f2 = b.request("render", view=1, format="png")
        open(os.path.join(OUT, "engine-tapped-frame.png"), "wb").write(f2["blob"]["data"])
        report["frames"]["tapped"] = {k: f2[k] for k in ("seq", "width", "height", "engineMs")}
        report["frames"]["tapped"]["png_bytes"] = len(f2["blob"]["data"])
        report["frames"]["tapped"]["png_dims"] = [f2["blob"]["width"], f2["blob"]["height"]]
        pids = descendants(proc.pid)
        report["cost"]["processes"] = len(pids)
        report["cost"]["pss_mib"] = round(sum(pss_kib(p) for p in pids) / 1024.0, 1)
        report["cost"]["pss_detail_kib"] = {str(p): pss_kib(p) for p in pids}
        report["assertions"] = {
            "asset_page_text_in_ax": "acos asset page" in names,
            "tap_changed_page_text": "tapped" in names,
            "js_interface_called": any(c.get("op") == "js.call" for c in b.jsi),
            "js_out_is_tapped": report["js_out"].get("valueJson") == '"tapped"',
        }
    finally:
        try:
            b.request("view.destroy", view=1, timeout=5)
        except Exception:
            pass
        session.close()
        proc.terminate()
        try:
            proc.wait(10)
        except subprocess.TimeoutExpired:
            proc.kill()
    open(os.path.join(OUT, "engine-probe.json"), "w").write(json.dumps(report, indent=2) + "\n")
    print(json.dumps(report, indent=2))
    ok = all(report.get("assertions", {}).values())
    print("verdict: %s" % ("pass" if ok else "fail"))
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())
