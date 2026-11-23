#!/usr/bin/env python3
"""Drive one `acos-art session` over its unix socket and check a counter flow.

The session serves one JSON reply line per request line; this script is the
caller side only (no guest code runs here), so it needs no process guard.

Sequence (all coordinates come from the *returned* tree, never hardcoded):

    tree                       -> find :id/inc and :id/reset bounds
    screenshot                 -> frame 1 hash
    tap(inc center)            -> expect text 0 -> 1, new frame hash
    tree / screenshot          -> the tap's own frame (fresh pixels)
    tap(inc center)            -> expect 1 -> 2, another new hash
    tap(reset center)          -> expect 2 -> 0
    screenshot                 -> fresh pixels after reset
    close

Exit code 0 only when every expectation holds; the JSON report says which.
"""

import argparse
import hashlib
import json
import os
import socket
import sys
import time


def connect(path, timeout):
    deadline = time.time() + timeout
    last = None
    while time.time() < deadline:
        try:
            s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
            s.connect(path)
            return s
        except OSError as e:  # not bound yet
            last = e
            time.sleep(0.02)
    raise SystemExit(f"could not connect to {path}: {last}")


class Session:
    def __init__(self, sock):
        self.sock = sock
        self.buf = b""

    def send(self, obj):
        line = json.dumps(obj, separators=(",", ":")) + "\n"
        t0 = time.perf_counter()
        self.sock.sendall(line.encode())
        return t0

    def reply(self):
        while b"\n" not in self.buf:
            chunk = self.sock.recv(1 << 16)
            if not chunk:
                raise SystemExit("session closed the socket before replying")
            self.buf += chunk
        line, self.buf = self.buf.split(b"\n", 1)
        return json.loads(line)

    def call(self, obj):
        t0 = self.send(obj)
        r = self.reply()
        return r, (time.perf_counter() - t0) * 1000.0


def find_nodes(node, out, pred):
    if pred(node):
        out.append(node)
    for c in node.get("children") or []:
        find_nodes(c, out, pred)


def by_id(tree, suffix):
    found = []
    find_nodes(tree, found, lambda n: str(n.get("id", "")).endswith(suffix))
    if not found:
        raise SystemExit(f"no node whose id ends with {suffix!r} in the tree")
    return found[0]


def bounds(node):
    import re

    m = re.match(r"\[(-?\d+),(-?\d+)\]\[(-?\d+),(-?\d+)\]", node["bounds"])
    if not m:
        raise SystemExit(f"unparseable bounds {node['bounds']!r}")
    return tuple(int(g) for g in m.groups())


def center(node):
    x1, y1, x2, y2 = bounds(node)
    return (x1 + x2) // 2, (y1 + y2) // 2


def sha256_file(path):
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--socket", required=True)
    ap.add_argument("--out", required=True, help="where per-step evidence goes")
    ap.add_argument("--json", default=None, help="write the report here")
    ap.add_argument("--startup-timeout", type=float, default=120.0)
    ap.add_argument("--reply-timeout", type=float, default=120.0)
    args = ap.parse_args()

    os.makedirs(args.out, exist_ok=True)
    sock = connect(args.socket, args.startup_timeout)
    sock.settimeout(args.reply_timeout)
    sess = Session(sock)

    steps = []
    failures = []

    def step(name, req, extra=None):
        r, ms = sess.call(req)
        rec = {
            "step": name,
            "request": req,
            "ok": bool(r.get("ok")),
            "cmd": r.get("cmd"),
            "frame_seq": r.get("frame_seq"),
            "md5": r.get("md5"),
            "png": r.get("png"),
            "guest_ms": r.get("guest_ms"),
            "host_ms": r.get("host_ms"),
            "wall_ms": round(ms, 3),
        }
        for k in ("tap", "key", "code", "error"):
            if k in r:
                rec[k] = r[k]
        if "tree" in r:
            texts = []
            find_nodes(r["tree"], texts, lambda n: bool(n.get("text")))
            rec["texts"] = [n["text"] for n in texts]
            rec["tree_sha256"] = hashlib.sha256(
                json.dumps(r["tree"], sort_keys=True).encode()
            ).hexdigest()
        if extra:
            rec.update(extra)
        if rec.get("png") and os.path.isfile(rec["png"]):
            rec["png_sha256"] = sha256_file(rec["png"])
            rec["png_bytes"] = os.path.getsize(rec["png"])
        steps.append(rec)
        return r, rec

    def fail(msg):
        failures.append(msg)
        print(f"FAIL {msg}", file=sys.stderr)

    # The caller owns the connection: the first line is ours, not the ready
    # line (that one went to the session's stdout before we connected).
    r, rec = step("tree", {"cmd": "tree"})
    tree = r.get("tree") or {}
    inc = by_id(tree, ":id/inc")
    reset = by_id(tree, ":id/reset")
    count = by_id(tree, ":id/count")
    inc_xy = center(inc)
    reset_xy = center(reset)
    rec["inc_bounds"] = inc["bounds"]
    rec["reset_bounds"] = reset["bounds"]
    rec["count_bounds"] = count["bounds"]
    rec["inc_center"] = list(inc_xy)
    rec["reset_center"] = list(reset_xy)

    def count_text(r):
        nodes = []
        find_nodes(r.get("tree") or {}, nodes, lambda n: str(n.get("id", "")).endswith(":id/count"))
        return nodes[0]["text"] if nodes else None

    hashes = {}
    seqs = []

    def expect_text(label, r, want):
        got = count_text(r)
        if got != want:
            fail(f"{label}: count text {got!r}, expected {want!r}")
        return got

    r, rec = step("screenshot_initial", {"cmd": "screenshot"})
    expect_text("screenshot_initial", r, "0")
    first = (rec.get("md5"), rec.get("frame_seq"))
    hashes["initial"] = rec.get("md5")
    seqs.append(rec.get("frame_seq"))

    tap_ms = {}
    for i, want in ((1, "1"), (2, "2")):
        r, rec = step(f"tap_inc_{i}", {"cmd": "tap", "x": inc_xy[0], "y": inc_xy[1]})
        tap_ms[f"inc{i}"] = rec["wall_ms"]
        rec["inc_center"] = [inc_xy[0], inc_xy[1]]
        expect_text(f"tap_inc_{i}", r, want)
        if not (r.get("tap") or {}).get("handled"):
            fail(f"tap_inc_{i}: decor dispatchTouchEvent did not handle the tap: {r.get('tap')}")
        h = rec.get("md5")
        if h in hashes.values():
            fail(f"tap_inc_{i}: frame md5 {h} repeats an earlier frame")
        hashes[f"inc{i}"] = h
        seqs.append(rec.get("frame_seq"))
        # screenshot right after the tap must be the *same* frame (fresh pixels,
        # no re-render needed): the session never hands out a stale path.
        r2, rec2 = step(f"screenshot_after_inc{i}", {"cmd": "screenshot"})
        expect_text(f"screenshot_after_inc{i}", r2, want)
        if rec2.get("md5") != h:
            fail(
                f"screenshot_after_inc{i}: md5 {rec2.get('md5')} != tap frame {h} "
                "(state changed between the tap and the screenshot)"
            )
        if rec2.get("png") != rec.get("png"):
            fail(f"screenshot_after_inc{i}: png {rec2.get('png')} != {rec.get('png')}")

    r, rec = step("tap_reset", {"cmd": "tap", "x": reset_xy[0], "y": reset_xy[1]})
    tap_ms["reset"] = rec["wall_ms"]
    rec["reset_center"] = [reset_xy[0], reset_xy[1]]
    expect_text("tap_reset", r, "0")
    if not (r.get("tap") or {}).get("handled"):
        fail(f"tap_reset: decor dispatchTouchEvent did not handle the tap: {r.get('tap')}")
    hashes["reset"] = rec.get("md5")
    seqs.append(rec.get("frame_seq"))

    r, rec = step("screenshot_after_reset", {"cmd": "screenshot"})
    expect_text("screenshot_after_reset", r, "0")
    if rec.get("md5") != hashes["reset"]:
        fail("screenshot_after_reset: not the frame the reset tap published")

    # frame_seq must advance monotonically, one step per published frame.
    if seqs != sorted(set(seqs)) or len(seqs) != len(set(seqs)):
        fail(f"frame_seq not strictly increasing: {seqs}")

    r, rec = step("close", {"cmd": "close"})
    if not r.get("ok"):
        fail(f"close: {r}")

    distinct = len({v for v in hashes.values() if v})
    if distinct != 4:
        fail(f"expected 4 distinct frame hashes (initial, inc1, inc2, reset), got {distinct}")

    report = {
        "socket": args.socket,
        "steps": steps,
        "frame_hashes": hashes,
        "frame_seqs": seqs,
        "distinct_frames": distinct,
        "tap_wall_ms": tap_ms,
        "failures": failures,
        "ok": not failures,
    }
    out = args.json or os.path.join(args.out, "drive.json")
    with open(out, "w") as f:
        json.dump(report, f, indent=2)
        f.write("\n")
    print(json.dumps({k: report[k] for k in ("ok", "frame_hashes", "frame_seqs", "tap_wall_ms", "failures")}, indent=2))
    return 0 if not failures else 1


if __name__ == "__main__":
    sys.exit(main())
