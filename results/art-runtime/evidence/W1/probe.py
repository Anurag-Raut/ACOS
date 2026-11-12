#!/usr/bin/env python3
"""D-M5: per-phase timeline of ONE `acos_observe {png:true, screenshot:path}`.

Holds a real `acos serve --engine art` session, launches the counter APK, then
issues N screenshot requests over the daemon's own socket with client-side
stamps, and joins them with the env-gated `DM5TRACE` trace lines that
`acos serve`/`acos-art` print on the same CLOCK_REALTIME clock.

Phases (all microseconds, realtime):
  client_write   probe t1 - t0            (sendall/flush of the request line)
  serve_read     cli.serve_request_read   (previous reply flush -> line parsed)
  serve_dispatch cli.serve_dispatch       (parse -> envelope built)
  art_roundtrip  cli.art_cmd_roundtrip    (stdin write -> acos-art reply line)
    art_composite   art.capture_composite (C1 blit/capture)
    art_encode      art.png_encode
    art_png_write   art.png_write
    art_md5         art.md5_file          (md5sum subprocess)
  png_read       cli.png_read             (serve reads the frame file back)
  shot_write     cli.serve_shot_write     (serve writes the caller's path)
  serve_reply    cli.serve_reply_write
  client_read    probe t3 - t2            (reply bytes into the client)

No product change: the trace is off unless DM5_TRACE names a file.
"""

import argparse
import json
import os
import socket
import subprocess
import sys
import time
from pathlib import Path

HERE = Path(__file__).resolve().parent
REPO = Path("/home/Qui/.paseo/worktrees/3oxa6xao/perf-w1-capture-mem")
sys.path.insert(0, str(REPO / "tools"))
sys.path.insert(0, str(REPO / "tools" / "art-runtime"))

APK = REPO / "fixtures/build/apps/counter.apk"


def now_us():
    return time.time_ns() // 1000


class Client:
    def __init__(self, path, timeout=180.0):
        self.sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        self.sock.settimeout(timeout)
        self.sock.connect(path)
        self.id = 0

    def call(self, tool, args):
        """One request with the write/reply split, and the reply's own timing."""
        self.id += 1
        req = {"id": self.id, "tool": tool, "args": args}
        line = (json.dumps(req, separators=(",", ":")) + "\n").encode()
        t0 = now_us()
        self.sock.sendall(line)
        t1 = now_us()
        buf = b""
        t2 = None
        while not buf.endswith(b"\n"):
            chunk = self.sock.recv(65536)
            if not chunk:
                raise ConnectionError("daemon closed during %s" % tool)
            if t2 is None:
                t2 = now_us()
            buf += chunk
        t3 = now_us()
        raw = json.loads(buf)
        return {
            "t0": t0, "t1": t1, "t2": t2, "t3": t3,
            "request_bytes": len(line), "reply_bytes": len(buf),
            "wall_ms": (t3 - t0) / 1000.0,
            "envelope": raw.get("envelope") or {},
        }


TRACE_EVENTS = (
    "cli.serve_request_read", "cli.serve_dispatch", "cli.art_cmd_write",
    "cli.art_cmd_roundtrip", "art.capture_composite", "art.png_encode",
    "art.png_write", "art.md5_file", "cli.png_read", "cli.serve_shot_write",
    "cli.serve_reply_write", "cli.serve_shot_write",
    "art.encode_filter", "art.encode_stored", "art.encode_adler32",
    "art.encode_idat_chunk", "art.encode_crc32", "art.encode_total",
    "c1.total", "c1.kernel_rows+sfgeom", "c1.memfd_read", "c1.blend",
    "c1.fnv_hash", "c1.canvas_alloc",
)


def parse_trace(path):
    """DM5TRACE lines -> {t_us: [events]} plus a flat list, in file order."""
    events = []
    if not Path(path).exists():
        return events
    with open(path, errors="replace") as fh:
        for line in fh:
            if not line.startswith("DM5TRACE "):
                continue
            fields = {}
            for tok in line.split():
                if "=" in tok:
                    k, v = tok.split("=", 1)
                    fields[k] = v
            ev = fields.get("ev")
            if ev not in TRACE_EVENTS:
                continue
            try:
                events.append({
                    "ev": ev,
                    "t_us": int(fields["t_us"]),
                    "dur_us": int(fields.get("dur_us", 0)),
                    "proc": fields.get("proc"),
                    "extra": {k: v for k, v in fields.items()
                              if k not in ("ev", "t_us", "dur_us", "proc")},
                })
            except (KeyError, ValueError):
                continue
    return events


def phase_table(samples, events):
    """Attach each sample's trace events to it (by realtime window)."""
    out = []
    for s in samples:
        lo, hi = s["t0"] - 2000, s["t3"] + 2000
        window = [e for e in events if s["t0"] < e["t_us"] < s["t3"]]
        by = {}
        for e in window:
            by.setdefault(e["ev"], []).append(e)
        def dur(name):
            vals = by.get(name) or []
            return max((v["dur_us"] for v in vals), default=None)
        env = s["envelope"]
        result = env.get("result") or {}
        frame = result.get("frame") or {}
        png = result.get("screenshot") or {}
        row = {
            "warmup": s.get("warmup"),
            "wall_ms": round(s["wall_ms"], 3),
            "ok": bool(env.get("ok")),
            "frame_seq": frame.get("frame_seq"),
            "previous_seq": frame.get("previous_seq"),
            "fresh": (isinstance(frame.get("frame_seq"), int)
                      and isinstance(frame.get("previous_seq"), int)
                      and frame["frame_seq"] > frame["previous_seq"]),
            "png_bytes": png.get("bytes"),
            "reply_bytes": s["reply_bytes"],
            "timing": (result.get("timing") or {}),
            "phases_us": {
                "client_write": s["t1"] - s["t0"],
                "serve_read": dur("cli.serve_request_read"),
                "serve_dispatch": dur("cli.serve_dispatch"),
                "art_roundtrip": dur("cli.art_cmd_roundtrip"),
                "art_composite": dur("art.capture_composite"),
                "c1_total": dur("c1.total"),
                "c1_kernel": dur("c1.kernel_rows+sfgeom"),
                "c1_memfd_read": dur("c1.memfd_read"),
                "c1_blend": dur("c1.blend"),
                "c1_fnv_hash": dur("c1.fnv_hash"),
                "c1_canvas_alloc": dur("c1.canvas_alloc"),
                "art_encode": dur("art.png_encode"),
                "art_png_write": dur("art.png_write"),
                "art_md5": dur("art.md5_file"),
                "png_read": dur("cli.png_read"),
                "shot_write": dur("cli.serve_shot_write"),
                "serve_reply": dur("cli.serve_reply_write"),
                "client_read": (s["t3"] - s["t2"]) if s["t2"] else None,
                "encode_filter": dur("art.encode_filter"),
                "encode_stored": dur("art.encode_stored"),
                "encode_adler32": dur("art.encode_adler32"),
                "encode_idat_chunk": dur("art.encode_idat_chunk"),
                "encode_crc32_idat": max(
                    (e["dur_us"] for e in by.get("art.encode_crc32") or []
                     if e["extra"].get("chunk") == "IDAT"), default=None),
            },
            "trace_events": window,
        }
        # The pipe/launcher IPC left over once the art-side phases are removed.
        rt = row["phases_us"]["art_roundtrip"]
        inner = sum(row["phases_us"][k] or 0 for k in
                    ("art_composite", "art_encode", "art_png_write", "art_md5"))
        row["phases_us"]["art_ipc_remainder"] = (rt - inner) if rt is not None else None
        out.append(row)
    return out


def run(args):
    scratch = Path(args.outdir)
    scratch.mkdir(parents=True, exist_ok=True)
    screens = scratch / "screens"
    screens.mkdir(exist_ok=True)
    sock = scratch / "serve.sock"
    for stale in (sock,):
        if stale.exists():
            stale.unlink()
    log = open(scratch / "serve.log", "w")
    env = dict(os.environ,
               ACOS_RUNTIME_DIR=str(scratch / "runtime"),
               ACOS_CACHE_DIR=str(scratch / "cache"),
               ACOS_SERVE_ARTIFACTS=str(scratch / "sessions"))
    trace_path = scratch / "trace.log"
    if trace_path.exists():
        trace_path.unlink()
    if args.trace:
        env["DM5_TRACE"] = str(trace_path)
    argv = [args.acos, "serve", "--socket", str(sock), "--engine", "art",
            "--idle-timeout", "900", "--max-sessions", "8"]
    doc = {"schema": "acos-dm5-probe/1", "binary": args.acos,
           "samples": args.samples, "warmup": args.warmup,
           "trace": bool(args.trace), "argv": argv}
    proc = subprocess.Popen(argv, cwd=str(REPO), env=env,
                            stdin=subprocess.DEVNULL,
                            stdout=open(scratch / "serve.stdout", "w"),
                            stderr=log, start_new_session=True)
    doc["serve_pid"] = proc.pid
    client = None
    try:
        deadline = time.time() + 30
        while time.time() < deadline and not sock.exists():
            if proc.poll() is not None:
                raise RuntimeError("serve died before binding rc=%s" % proc.returncode)
            time.sleep(0.05)
        if not sock.exists():
            raise RuntimeError("serve never bound %s" % sock)
        client = Client(str(sock))
        base = {"request_id": "dm5", "session_id": "s_dm5"}
        client.call("ping", {})
        launch = client.call("acos_launch", {"base": base, "apk": str(APK)})
        if not launch["envelope"].get("ok"):
            raise RuntimeError("launch failed: %s" % (launch["envelope"].get("error"),))
        time.sleep(args.settle)
        samples = []
        for i in range(args.warmup + args.samples):
            path = screens / ("m5-%03d.png" % i)
            r = client.call("acos_observe", {"base": base, "tree": False,
                                             "text": False, "png": True,
                                             "screenshot": str(path)})
            r["warmup"] = i < args.warmup
            r["path"] = str(path)
            r["png_on_disk"] = path.stat().st_size if path.exists() else 0
            samples.append(r)
        doc["samples_raw"] = samples
        doc["ok"] = True
    except Exception as exc:  # noqa: BLE001
        doc["error"] = "%s: %s" % (type(exc).__name__, exc)
        doc["ok"] = False
    finally:
        if client is not None:
            try:
                client.call("acos_destroy", {"base": {
                    "request_id": "dm5-destroy", "session_id": "s_dm5"}})
            except Exception:  # noqa: BLE001
                pass
            client.sock.close()
        if proc.poll() is None:
            proc.terminate()
            try:
                proc.wait(timeout=20)
            except subprocess.TimeoutExpired:
                proc.kill()
        log.close()
    events = parse_trace(trace_path)
    doc["trace_events"] = len(events)
    doc["rows"] = phase_table([s for s in doc.get("samples_raw", [])], events)
    with open(Path(args.json) if args.json else scratch / "probe.json", "w") as fh:
        json.dump(doc, fh, indent=2)
    ok = sum(1 for r in doc["rows"] if r["ok"] and r["fresh"] and r["warmup"] is False)
    print("probe: ok=%s rows=%d fresh_ok=%d trace_events=%d"
          % (doc.get("ok"), len(doc["rows"]), ok, len(events)))
    if doc.get("error"):
        print("probe error: %s" % doc["error"])
    return doc


def main(argv=None):
    ap = argparse.ArgumentParser()
    ap.add_argument("--acos", required=True)
    ap.add_argument("--outdir", required=True)
    ap.add_argument("--samples", type=int, default=22)
    ap.add_argument("--warmup", type=int, default=2)
    ap.add_argument("--settle", type=float, default=5.0)
    ap.add_argument("--json")
    ap.add_argument("--trace", action="store_true", default=True)
    ap.add_argument("--no-trace", dest="trace", action="store_false")
    args = ap.parse_args(argv)
    doc = run(args)
    return 0 if doc.get("ok") else 1


if __name__ == "__main__":
    sys.exit(main())
