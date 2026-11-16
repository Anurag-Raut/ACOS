#!/usr/bin/env python3
"""WV4 minimal WebView session listener (test only — NOT a broker).

The acceptance test for the guest path ⇄ host path mapping: it listens on the
host path the session exposes as `/run/acos/webview.sock` (WIRE.md §0) and
answers the guest provider's `hello` with a syntactically valid WIRE v1 reply,
so the guest's `LocalSocketImpl.connectLocal` → `connect(2)` must have really
succeeded for anything to be printed.

It is deliberately dumb: every other request is answered `ok` with an empty
result and logged. It has no engine, no Chromium, no pages, and it never
pretends to have one — the HTML/page work is the host broker's
(`crates/acos-webview`, WVB).

Usage: listener.py <host-socket-path> [log-file]
"""
import json
import os
import socket
import struct
import sys
import threading

MAX_FRAME = 64 * 1024 * 1024


def read_exact(stream, n):
    buf = b""
    while len(buf) < n:
        chunk = stream.recv(n - len(buf))
        if not chunk:
            raise EOFError("peer closed")
        buf += chunk
    return buf


def log(path, line):
    print(line, flush=True)
    if path:
        with open(path, "a", encoding="utf-8") as fh:
            fh.write(line + "\n")


def main():
    sock_path = sys.argv[1]
    log_path = sys.argv[2] if len(sys.argv) > 2 else None
    if os.path.exists(sock_path):
        os.unlink(sock_path)
    server = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    server.bind(sock_path)
    os.chmod(sock_path, 0o666)
    server.listen(1)
    log(log_path, "listener: bound %s" % sock_path)

    conn, _ = server.accept()
    log(log_path, "listener: guest connected")
    while True:
        try:
            header = read_exact(conn, 4)
        except EOFError:
            log(log_path, "listener: guest closed")
            return
        length = struct.unpack(">I", header)[0]
        if length == 0 or length > MAX_FRAME:
            log(log_path, "listener: BAD_FRAME length=%d" % length)
            return
        body = read_exact(conn, length)
        kind = body[0]
        if kind != 1:
            log(log_path, "listener: non-JSON frame kind=%d" % kind)
            continue
        msg = json.loads(body[1:].decode("utf-8"))
        log(log_path, "guest->host %s" % json.dumps(msg, sort_keys=True))

        if "event" in msg:
            continue
        reply = {"id": msg.get("id", "g0"), "ok": True, "result": {}}
        if msg.get("op") == "hello":
            reply["result"] = {
                "proto": 1,
                "session": "wv4-test-listener",
                "engine": {
                    "product": "acos-wv4-test-listener",
                    "version": "0",
                    "revision": "none",
                    "protocolVersion": "1.3",
                    "userAgent": "acos-wv4-test-listener/0",
                },
                "limits": {"maxViews": 1, "maxFrameBytes": MAX_FRAME},
            }
        elif msg.get("op") == "view.create":
            reply["result"] = {
                "view": msg.get("view"),
                "width": msg.get("width"),
                "height": msg.get("height"),
                "density": msg.get("density"),
            }
        payload = json.dumps(reply).encode("utf-8")
        frame = struct.pack(">I", len(payload) + 1) + b"\x01" + payload
        conn.sendall(frame)
        log(log_path, "host->guest %s" % json.dumps(reply, sort_keys=True))


if __name__ == "__main__":
    threading.current_thread().name = "wv4-listener"
    main()
