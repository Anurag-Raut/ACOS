#!/usr/bin/env python3
"""Live-view stream probe (VIEW1a): subscribe to an ART session's stream socket.

This is the reference consumer of protocol v1 (`docs/LIVE_VIEW.md`). It saves the
frames it receives and the action/event feed to a directory, then prints a JSON
summary on stdout, so a test can assert on both without decoding images.

    tools/view/stream_probe.py --socket /run/user/1000/acos-art-1-0/view.sock \
        --out .cache/view1-probe/run1 --frames 4 --seconds 20

Exit status is 0 when the hello arrived (and, with `--frames N`, when N frames
arrived), 1 otherwise. Standard library only: it runs anywhere `acos` runs.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import socket
import struct
import sys
import time

TYPE_HELLO = b"H"
TYPE_FRAME = b"F"
TYPE_EVENT = b"E"

DEFAULT_MAX_FPS = 15
DEFAULT_SCALE = 0.5
DEFAULT_FORMAT = "png"


def recv_exact(sock: socket.socket, count: int) -> bytes | None:
    """Read exactly `count` bytes, or None at a clean end of stream."""
    chunks = []
    remaining = count
    while remaining:
        chunk = sock.recv(remaining)
        if not chunk:
            return None
        chunks.append(chunk)
        remaining -= len(chunk)
    return b"".join(chunks)


def read_message(sock: socket.socket) -> tuple[bytes, bytes] | None:
    """One protocol message: 4-byte big-endian length, type byte, payload."""
    header = recv_exact(sock, 4)
    if header is None:
        return None
    (length,) = struct.unpack(">I", header)
    if length == 0:
        raise ValueError("zero-length protocol message")
    body = recv_exact(sock, length)
    if body is None:
        return None
    return body[:1], body[1:]


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--socket", required=True, help="the session's stream_socket")
    parser.add_argument("--out", required=True, help="directory for frames and events")
    parser.add_argument("--frames", type=int, default=0, help="stop after N frames (0 = no cap)")
    parser.add_argument("--seconds", type=float, default=30.0, help="give up after this long")
    parser.add_argument(
        "--idle-exit",
        type=float,
        default=0.0,
        help="exit this many seconds after the last message (0 = off); this is how a "
        "probe that watches a live session ends by itself",
    )
    parser.add_argument("--format", default=DEFAULT_FORMAT, choices=["jpeg", "png"])
    parser.add_argument("--scale", type=float, default=DEFAULT_SCALE)
    parser.add_argument("--max-fps", type=int, default=DEFAULT_MAX_FPS)
    parser.add_argument("--no-frames", action="store_true", help="subscribe to events only")
    parser.add_argument("--no-events", action="store_true", help="subscribe to frames only")
    parser.add_argument("--save-frames", action="store_true", help="write the image bytes to disk")
    args = parser.parse_args(argv)

    os.makedirs(args.out, exist_ok=True)
    events_path = os.path.join(args.out, "events.jsonl")
    frames_path = os.path.join(args.out, "frames.jsonl")
    started = time.time()

    sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    poll = max(1.0, args.seconds)
    if args.idle_exit > 0:
        poll = min(poll, max(0.2, args.idle_exit))
    sock.settimeout(poll)
    last_message = time.time()
    sock.connect(args.socket)
    subscribe = {
        "v": 1,
        "subscribe": {
            "frames": not args.no_frames,
            "events": not args.no_events,
            "max_fps": args.max_fps,
            "scale": args.scale,
            "format": args.format,
        },
    }
    sock.sendall((json.dumps(subscribe) + "\n").encode("utf-8"))

    hello = None
    frames: list[dict] = []
    events: list[dict] = []
    dropped = 0
    with open(events_path, "w", encoding="utf-8") as events_file, open(
        frames_path, "w", encoding="utf-8"
    ) as frames_file:
        while True:
            if args.frames and len(frames) >= args.frames:
                break
            if time.time() - started > args.seconds:
                break
            try:
                message = read_message(sock)
            except (socket.timeout, TimeoutError):
                if args.idle_exit > 0 and time.time() - last_message >= args.idle_exit:
                    break
                continue
            if message is None:
                break
            last_message = time.time()
            type_byte, payload = message
            if type_byte == TYPE_HELLO:
                hello = json.loads(payload.decode("utf-8"))
                print(json.dumps({"hello": hello}), file=sys.stderr)
            elif type_byte == TYPE_FRAME:
                newline = payload.find(b"\n")
                if newline < 0:
                    raise ValueError("frame message has no header line")
                header = json.loads(payload[:newline].decode("utf-8"))
                image = payload[newline + 1 :]
                record = dict(header)
                record["bytes"] = len(image)
                record["sha256"] = hashlib.sha256(image).hexdigest()
                if args.save_frames:
                    extension = "jpg" if header.get("format") == "jpeg" else "png"
                    name = "frame-%05d.%s" % (len(frames) + 1, extension)
                    with open(os.path.join(args.out, name), "wb") as image_file:
                        image_file.write(image)
                    record["path"] = os.path.join(args.out, name)
                frames.append(record)
                frames_file.write(json.dumps(record) + "\n")
                frames_file.flush()
            elif type_byte == TYPE_EVENT:
                event = json.loads(payload.decode("utf-8"))
                events.append(event)
                events_file.write(json.dumps(event) + "\n")
                events_file.flush()
            else:
                dropped += 1

    summary = {
        "socket": args.socket,
        "hello": hello,
        "frames": frames,
        "frame_count": len(frames),
        "distinct_frames": len({frame["sha256"] for frame in frames}),
        "events": events,
        "event_count": len(events),
        "unknown_messages": dropped,
        "elapsed_ms": int((time.time() - started) * 1000),
        "out": args.out,
    }
    print(json.dumps(summary))
    ok = hello is not None and (not args.frames or len(frames) >= args.frames)
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
