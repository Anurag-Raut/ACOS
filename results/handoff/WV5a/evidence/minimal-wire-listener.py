#!/usr/bin/env python3
"""WV5a minimal WIRE-v1 session listener (test harness only — NOT a broker).

This is the acceptance instrument for wall 9: it listens on the host path the
session exposes as `/run/acos/webview.sock` (WIRE.md §0) and speaks *only* the
normative v1 framing (results/handoff/WVB/WIRE.md §1-§2), so everything it logs
had to cross the guest's real `connect(2)`.

It is deliberately engine-free: it has no Chromium, no pages and no HTML. What it
does:

* answers `hello` with a syntactically valid v1 reply (so the provider's session
  handshake completes),
* records the **order** of the requests the guest provider makes and fails
  (nonzero exit + `verdict: fail`) unless the first four are
  `hello`, `view.create`, `view.setViewport`, `view.loadUrl` — the wall-9 walk
  (construct -> attach -> measure -> layout -> load),
* answers `render` with the JSON response **and** a real 16x16 PNG BLOB
  (`WIRE.md` §1 kind 2, §4.2), because `render` is the one op that is two frames
  and a listener that answered only JSON would hide a BLOB-pairing bug,
* answers every other request `ok` with an empty result, and logs it. It never
  claims to have done engine work it has not done.

Usage: minimal-wire-listener.py <host-socket-path> [log-file] [--verdict FILE]
"""

import json
import os
import socket
import struct
import sys
import zlib

MAX_FRAME = 64 * 1024 * 1024
EXPECTED = ("hello", "view.create", "view.setViewport", "view.loadUrl")

LOG = None
VERDICT = None


def log(line):
    print(line, flush=True)
    if LOG:
        with open(LOG, "a", encoding="utf-8") as handle:
            handle.write(line + "\n")


def verdict(status, detail):
    line = "verdict: %s (%s)" % (status, detail)
    log(line)
    if VERDICT:
        with open(VERDICT, "w", encoding="utf-8") as handle:
            handle.write(line + "\n")


def png(size=16):
    """A `size`x`size` RGBA PNG of a solid colour (no PIL, no engine)."""
    raw = b"".join(b"\x00" + b"\x20\x80\xc0\xff" * size for _ in range(size))

    def chunk(kind, data):
        body = kind + data
        return struct.pack(">I", len(data)) + body + struct.pack(">I", zlib.crc32(body))

    ihdr = struct.pack(">IIBBBBB", size, size, 8, 6, 0, 0, 0)
    return (
        b"\x89PNG\r\n\x1a\n"
        + chunk(b"IHDR", ihdr)
        + chunk(b"IDAT", zlib.compress(raw, 9))
        + chunk(b"IEND", b"")
    )


def read_exact(stream, n):
    buf = b""
    while len(buf) < n:
        chunk = stream.recv(n - len(buf))
        if not chunk:
            raise EOFError("peer closed")
        buf += chunk
    return buf


def send_json(conn, message):
    payload = json.dumps(message).encode("utf-8")
    conn.sendall(struct.pack(">I", len(payload) + 1) + b"\x01" + payload)
    log("host->guest %s" % json.dumps(message, sort_keys=True))


def send_blob(conn, view, seq, data, width, height, fmt=0):
    nid = b"1"
    body = (
        b"\x02"
        + struct.pack(">I", len(nid))
        + nid
        + struct.pack(">B", fmt)
        + struct.pack(">II", width, height)
        + struct.pack(">I", len(data))  # stride: for png, the byte length
        + struct.pack(">Q", seq)
        + data
    )
    conn.sendall(struct.pack(">I", len(body)) + body)
    log(
        "host->guest BLOB view=%s seq=%d format=%d %dx%d bytes=%d (kind 2)"
        % (view, seq, fmt, width, height, len(data))
    )


def main():
    global LOG, VERDICT
    args = [a for a in sys.argv[1:] if not a.startswith("--verdict=")]
    for arg in sys.argv[1:]:
        if arg.startswith("--verdict="):
            VERDICT = arg.split("=", 1)[1]
    sock_path = args[0]
    LOG = args[1] if len(args) > 1 else None
    if os.path.exists(sock_path):
        os.unlink(sock_path)
    server = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    server.bind(sock_path)
    os.chmod(sock_path, 0o666)
    server.listen(1)
    log("listener: bound %s" % sock_path)

    conn, _ = server.accept()
    log("listener: guest connected")
    seen = []
    sizes = []
    seq = 0
    frame_png = png()
    while True:
        try:
            length = struct.unpack(">I", read_exact(conn, 4))[0]
        except EOFError:
            log("listener: guest closed")
            break
        if length == 0 or length > MAX_FRAME:
            verdict("fail", "BAD_FRAME length=%d" % length)
            return 1
        body = read_exact(conn, length)
        if body[0] != 1:
            log("listener: non-JSON frame kind=%d" % body[0])
            continue
        msg = json.loads(body[1:].decode("utf-8"))
        if "event" in msg:
            log("guest->host event %s" % json.dumps(msg, sort_keys=True))
            continue
        op = msg.get("op")
        seen.append(op)
        log("guest->host [%d] %s" % (len(seen), json.dumps(msg, sort_keys=True)))

        reply = {"id": msg.get("id", "g0"), "ok": True, "result": {}}
        if op == "hello":
            reply["result"] = {
                "proto": 1,
                "session": "wv5a-test-listener",
                "engine": {
                    "product": "acos-wv5a-test-listener",
                    "version": "0",
                    "revision": "none",
                    "protocolVersion": "1.3",
                    "userAgent": "acos-wv5a-test-listener/0",
                },
                "limits": {"maxViews": 8, "maxFrameBytes": MAX_FRAME},
            }
        elif op == "view.create":
            log(
                "listener: create view=%s %sx%s density=%s"
                % (
                    msg.get("view"),
                    msg.get("width"),
                    msg.get("height"),
                    msg.get("density"),
                )
            )
            sizes.append((op, msg.get("width"), msg.get("height")))
            reply["result"] = {
                "view": msg.get("view"),
                "width": msg.get("width"),
                "height": msg.get("height"),
                "density": msg.get("density"),
            }
        elif op == "view.setViewport":
            log(
                "listener: resize view=%s %sx%s density=%s"
                % (
                    msg.get("view"),
                    msg.get("width"),
                    msg.get("height"),
                    msg.get("density"),
                )
            )
            sizes.append((op, msg.get("width"), msg.get("height")))
            reply["result"] = {
                "view": msg.get("view"),
                "width": msg.get("width"),
                "height": msg.get("height"),
                "density": msg.get("density"),
            }
        elif op == "view.loadUrl":
            log("listener: load view=%s url=%s" % (msg.get("view"), msg.get("url")[:80]))
            reply["result"] = {
                "view": msg.get("view"),
                "url": msg.get("url"),
                "loadMs": 0,
            }
        elif op == "render":
            seq += 1
            reply["result"] = {
                "view": msg.get("view"),
                "seq": seq,
                "width": 16,
                "height": 16,
                "density": 1.0,
                "format": "png",
                "bytes": len(frame_png),
                "damage": {"x": 0, "y": 0, "w": 16, "h": 16},
                "engineMs": 0,
                "hasDamage": True,
            }
            send_json(conn, reply)
            send_blob(conn, msg.get("view"), seq, frame_png, 16, 16)
            continue
        send_json(conn, reply)

    # The wall-9 walk, as a subsequence: `view.create` at measure,
    # `view.setViewport` at layout, `view.loadUrl` after the view is laid out.
    # Focus/visibility requests may interleave; they are logged above and are
    # not part of the ordering claim.
    position = 0
    for op in seen:
        if position < len(EXPECTED) and op == EXPECTED[position]:
            position += 1
    if position != len(EXPECTED):
        verdict(
            "fail",
            "request order was %s, expected %s as a subsequence"
            % (seen, list(EXPECTED)),
        )
        return 1
    # The very first frame must be the session handshake (WIRE.md §6).
    if not seen or seen[0] != "hello":
        verdict("fail", "the first request was not hello: %s" % (seen[:1],))
        return 1
    # The first create and the first resize must carry the View's real size
    # (later viewports are the layout's own business: the status TextView above
    # the WebView grows, so the ScrollView re-measures the WebView to 900x614 and
    # then back to 900x700, and those are real resizes too).
    if len(sizes) < 2 or sizes[0][0] != "view.create" or sizes[1][0] != "view.setViewport":
        verdict("fail", "no view.create + view.setViewport pair: %s" % (sizes,))
        return 1
    for op, width, height in sizes[:2]:
        if (width, height) != (900, 700):
            verdict(
                "fail",
                "%s carried %sx%s, expected the View's own 900x700"
                % (op, width, height),
            )
            return 1
    verdict(
        "pass",
        "ordered %s (all requests: %s); geometry 900x700" % (list(EXPECTED), seen),
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
