#!/usr/bin/env python3
"""Drive one real-world APK through ACOS's real-ART engine and record the ladder.

One app per invocation, one `acos serve --engine art` daemon per app, **one
connection held open for the whole run** (the guest is currently killed when the
launch connection closes — GL1 owns that bug; this harness must not depend on
the fix landing). The caller wraps this script in
`tools/art-runtime/run-exclusive.sh --task K1 -- tools/guarded.sh ...` so the
host-wide ART lock is taken and released per app.

Steps (each one recorded with its own wall time and structured error):

    inspect       `acos inspect` — static, no guest
    launch        acos_launch  -> first frame (20 s budget; the launch reply's
                                  stage_reached is the honest ladder rung)
    tree          acos_observe_tree
    screenshot    acos_observe {screenshot: <path>}
    tap           acos_tap at the centre of the first clickable node
    tree_after    acos_observe_tree again

Nothing from the APK is executed outside the ACOS guest. The output directory
gets the envelopes, the serve log, the ART stage markers, and one PNG.

Usage:
  tools/compat/k1-run.py --apk X.apk --name X --out DIR [--json report.json]
"""

import argparse
import hashlib
import json
import os
import re
import signal
import socket
import subprocess
import sys
import threading
import time

HERE = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.abspath(os.path.join(HERE, "..", ".."))
CACHE = os.environ.get("ACOS_K1_RUN_CACHE", "/home/Qui/Projects/acos/.cache/k1")


def log(msg):
    print(f"[k1] {msg}", file=sys.stderr, flush=True)


def acos_bin():
    return os.environ.get("ACOS_BIN", os.path.join(CACHE, "bin", "acos"))


def art_bin():
    return os.environ.get("ACOS_ART_BIN", os.path.join(CACHE, "bin", "acos-art"))


class Client:
    def __init__(self, sock):
        self.sock = sock
        self.buf = b""
        self.n = 0

    def call(self, tool, args, timeout):
        self.n += 1
        rid = f"req_k1_{self.n}"
        body = dict(args)
        base = dict(body.get("base") or {})
        base["request_id"] = rid
        body["base"] = base
        req = {"id": rid, "tool": tool, "args": body}
        self.sock.settimeout(timeout)
        t0 = time.perf_counter()
        self.sock.sendall((json.dumps(req, separators=(",", ":")) + "\n").encode())
        try:
            while b"\n" not in self.buf:
                chunk = self.sock.recv(1 << 18)
                if not chunk:
                    raise RuntimeError("daemon closed the socket before replying")
                self.buf += chunk
        except socket.timeout:
            raise TimeoutError(f"{tool}: no reply within {timeout:.0f}s")
        line, self.buf = self.buf.split(b"\n", 1)
        wall_ms = (time.perf_counter() - t0) * 1000.0
        reply = json.loads(line)
        return reply.get("envelope") or {}, round(wall_ms, 1), rid


def descendants(pid):
    """All live descendant pids of `pid` (best effort, /proc walk)."""
    children = {}
    for entry in os.listdir("/proc"):
        if not entry.isdigit():
            continue
        try:
            with open(f"/proc/{entry}/stat") as f:
                fields = f.read().rsplit(") ", 1)[-1].split()
            ppid = int(fields[1])
        except (OSError, IndexError, ValueError):
            continue
        children.setdefault(ppid, []).append(int(entry))
    out, stack = [], [pid]
    while stack:
        p = stack.pop()
        for c in children.get(p, []):
            out.append(c)
            stack.append(c)
    return out


def rss_kb(pid):
    """Peak RSS (VmHWM) and current RSS (VmRSS) of a pid, in KiB."""
    hwm = rss = 0
    try:
        with open(f"/proc/{pid}/status") as f:
            for line in f:
                if line.startswith("VmHWM:"):
                    hwm = int(line.split()[1])
                elif line.startswith("VmRSS:"):
                    rss = int(line.split()[1])
    except OSError:
        pass
    return hwm, rss


class Sampler(threading.Thread):
    """Samples the daemon's process tree while the app runs."""

    def __init__(self, pid, interval=0.1):
        super().__init__(daemon=True)
        self.pid = pid
        self.interval = interval
        self.stop = threading.Event()
        self.peak_kb = 0
        self.peak_at = None

    def run(self):
        while not self.stop.is_set():
            total = 0
            pids = [self.pid] + descendants(self.pid)
            for p in pids:
                hwm, _ = rss_kb(p)
                total += hwm
            if total > self.peak_kb:
                self.peak_kb = total
                self.peak_at = round(time.time(), 2)
            self.stop.wait(self.interval)


def sha256(path):
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def roots(tree):
    """Accept both tree shapes: {"nodes": [...]} (serve replies) and a node."""
    if not isinstance(tree, dict):
        return []
    if isinstance(tree.get("nodes"), list):
        return tree["nodes"]
    return [tree]


def find_nodes(tree, out, pred):
    stack = list(roots(tree))
    while stack:
        node = stack.pop()
        if pred(node):
            out.append(node)
        stack.extend(node.get("children") or [])


def parse_bounds(s):
    m = re.match(r"\[(-?\d+),(-?\d+)\]\[(-?\d+),(-?\d+)\]", s or "")
    return tuple(int(g) for g in m.groups()) if m else None


def first_clickable(tree):
    nodes = []
    find_nodes(tree or {}, nodes, lambda n: bool(n.get("clickable")))
    for n in nodes:
        b = parse_bounds(n.get("bounds"))
        if b and b[2] - b[0] >= 8 and b[3] - b[1] >= 8:
            return n, b
    return None, None


def texts(tree, limit=40):
    out = []
    find_nodes(tree or {}, out, lambda n: bool(n.get("text")))
    return [n["text"] for n in out][:limit]


def error_of(env):
    e = env.get("error") or {}
    if not e:
        return None
    details = e.get("details") or {}
    return {
        "code": e.get("code"),
        "message": e.get("message"),
        "hint": e.get("hint"),
        "retryable": e.get("retryable"),
        "stage": details.get("stage"),
        "guest_signal": details.get("guest_signal"),
        "guest_exit": details.get("guest_exit"),
        "guest_crash": details.get("guest_crash"),
        "session_exit": details.get("session_exit"),
        "unsupported_api": details.get("api") or details.get("symbol"),
        "artifact": details.get("artifact") or details.get("path"),
        "stderr_tail": (details.get("stderr_tail") or "")[-4000:],
        "detail": details.get("detail"),
        "raw_details": {k: v for k, v in details.items() if k not in ("stderr_tail",)},
    }


def wait_socket(path, deadline_s, proc):
    deadline = time.time() + deadline_s
    last = None
    while time.time() < deadline:
        if proc.poll() is not None:
            raise RuntimeError(f"serve exited rc={proc.returncode} before listening")
        try:
            s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
            s.connect(path)
            return s
        except OSError as e:
            last = e
            time.sleep(0.02)
    raise TimeoutError(f"serve socket {path} never accepted ({last})")


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--apk", required=True)
    ap.add_argument("--name", required=True)
    ap.add_argument("--out", required=True)
    ap.add_argument("--json", default=None)
    ap.add_argument("--first-frame-timeout", type=float, default=20.0)
    ap.add_argument("--step-timeout", type=float, default=30.0)
    ap.add_argument("--keep-apk-basename", action="store_true", default=True)
    args = ap.parse_args()

    out = args.out
    os.makedirs(out, exist_ok=True)
    acos, art = acos_bin(), art_bin()
    for binary in (acos, art):
        if not os.access(binary, os.X_OK):
            raise SystemExit(f"missing engine binary: {binary}")

    report = {
        "app": args.name,
        "apk": os.path.abspath(args.apk),
        "apk_sha256": sha256(args.apk),
        "started_at": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
        "engine": "art",
        "stages": {},
        "steps": [],
        "first_frame": None,
        "time_to_first_frame_ms": None,
        "peak_rss_mib": None,
        "crash_class": None,
        "errors": [],
        "markers": {},
        "texts": [],
        "tap": None,
        "screenshot": None,
        "ok": False,
    }

    # ---- inspect (static; no guest, no ART lock needed) -------------------
    t0 = time.perf_counter()
    insp = subprocess.run(
        [acos, "inspect", args.apk, "--json"],
        capture_output=True,
        text=True,
        timeout=120,
    )
    insp_ms = round((time.perf_counter() - t0) * 1000, 1)
    try:
        insp_json = json.loads(insp.stdout)
    except json.JSONDecodeError:
        insp_json = {}
    with open(os.path.join(out, "00-inspect.json"), "w") as f:
        json.dump(insp_json, f, indent=1)
    result = insp_json.get("result") or {}
    inspect_ok = bool(insp_json.get("ok")) and bool(result.get("launcher_activity"))
    report["inspect"] = {
        "ok": inspect_ok,
        "wall_ms": insp_ms,
        "code": (insp_json.get("error") or {}).get("code"),
        "message": (insp_json.get("error") or {}).get("message"),
        "package": result.get("package"),
        "launcher_activity": result.get("launcher_activity"),
        "min_sdk": result.get("min_sdk"),
        "target_sdk": result.get("target_sdk"),
        "native_abis": result.get("native_abis"),
        "dex_files": len(result.get("dex_files") or []),
        "activity_count": len(((result.get("components") or {}).get("activities")) or []),
    }
    report["stages"]["inspect"] = {
        "ok": inspect_ok,
        "wall_ms": insp_ms,
        "error": report["inspect"]["message"] if not inspect_ok else None,
    }
    if not inspect_ok:
        report["errors"].append({"step": "inspect", **report["inspect"]})
        write_report(report, args, out)
        return 1

    # ---- serve (one daemon per app) --------------------------------------
    sock_path = os.path.join(CACHE, "s.sock")
    try:
        os.unlink(sock_path)
    except FileNotFoundError:
        pass
    artifacts = os.path.join(out, "art-sessions")
    os.makedirs(artifacts, exist_ok=True)
    env = dict(os.environ)
    env["ACOS_ART_BIN"] = art
    env["ACOS_SERVE_ARTIFACTS"] = artifacts
    env.setdefault(
        "ACOS_FONT_DIR", "/home/Qui/Projects/acos/.cache/boot/android-34/fonts"
    )
    env.pop("ACOS_BOOT_DIR", None)
    # Keep the ART stage ladder in the serve log: it is the evidence for which
    # rung a failure happened on when the guest writes no session-exit.json.
    env["ACOS_ART_STAGE_TIMING"] = "1"
    serve_log = open(os.path.join(out, "serve.log"), "w")
    serve = subprocess.Popen(
        [
            acos,
            "serve",
            "--socket",
            sock_path,
            "--json",
            "--engine",
            "art",
            "--idle-timeout",
            "30",
            "--max-sessions",
            "2",
        ],
        stdout=serve_log,
        stderr=subprocess.STDOUT,
        env=env,
        start_new_session=True,
    )
    sampler = Sampler(serve.pid)
    sampler.start()
    client = None
    try:
        s = wait_socket(sock_path, 30, serve)
        client = Client(s)

        # ---- launch / first frame ---------------------------------------
        try:
            env_launch, wall, rid = client.call(
                "acos_launch",
                {"apk": os.path.abspath(args.apk), "name": args.name},
                args.first_frame_timeout,
            )
            report["steps"].append(
                {"step": "launch", "request_id": rid, "wall_ms": wall,
                 "ok": bool(env_launch.get("ok")), "status": env_launch.get("status"),
                 "error": error_of(env_launch)}
            )
            write_json(os.path.join(out, "01-launch.json"), env_launch)
        except TimeoutError as e:
            env_launch = None
            report["steps"].append(
                {"step": "launch", "wall_ms": args.first_frame_timeout * 1000,
                 "ok": False, "status": "timeout",
                 "error": {"code": "HARNESS_TIMEOUT", "message": str(e), "stage": "first_frame"}}
            )
            report["stages"]["launch"] = {
                "ok": False,
                "reason": f"no reply within {args.first_frame_timeout:.0f}s (first-frame budget)",
            }
            report["crash_class"] = "timeout"

        if env_launch and env_launch.get("ok"):
            lres = env_launch.get("result") or {}
            report["time_to_first_frame_ms"] = wall
            report["stages"]["launch"] = {
                "ok": True,
                "wall_ms": wall,
                "reached_stage": lres.get("stage_reached"),
                "activity": lres.get("activity"),
                "package": lres.get("package")
                or (lres.get("tree") or {}).get("package")
                or report["inspect"].get("package"),
                "tree_node_count": lres.get("tree_node_count"),
                "frame": lres.get("frame"),
                "session_start_ms": (lres.get("timing") or {}).get("session_start_ms"),
            }
            report["stages"]["process_start"] = {"ok": True, "evidence": "launch pass"}
            report["stages"]["application_on_create"] = {"ok": True, "evidence": "launch pass"}
            report["stages"]["first_frame"] = {"ok": True, "wall_ms": wall}
            report["texts"] = texts(lres.get("tree") or {})

            # ---- tree ----------------------------------------------------
            try:
                env_tree, wall_t, rid = client.call(
                    "acos_observe_tree", {}, args.step_timeout
                )
                ok_tree = bool(env_tree.get("ok"))
                report["steps"].append(
                    {"step": "tree", "request_id": rid, "wall_ms": wall_t, "ok": ok_tree,
                     "status": env_tree.get("status"), "error": error_of(env_tree)}
                )
                write_json(os.path.join(out, "02-tree.json"), env_tree)
            except TimeoutError as e:
                ok_tree = False
                env_tree = None
                report["steps"].append(
                    {"step": "tree", "wall_ms": args.step_timeout * 1000, "ok": False,
                     "status": "timeout", "error": {"code": "HARNESS_TIMEOUT", "message": str(e)}}
                )
            tree_nodes = ((env_tree or {}).get("result") or {}).get("tree") or {}
            report["stages"]["tree"] = {
                "ok": ok_tree,
                "node_count": ((env_tree or {}).get("result") or {}).get("node_count"),
            }
            if ok_tree:
                report["texts"] = texts(tree_nodes)

            # ---- screenshot ----------------------------------------------
            png = os.path.join(out, "screen.png")
            ok_shot = False
            if ok_tree:
                try:
                    env_shot, wall_s, rid = client.call(
                        "acos_observe", {"screenshot": png}, args.step_timeout
                    )
                    ok_shot = bool(env_shot.get("ok"))
                    report["steps"].append(
                        {"step": "screenshot", "request_id": rid, "wall_ms": wall_s,
                         "ok": ok_shot, "status": env_shot.get("status"),
                         "error": error_of(env_shot)}
                    )
                    write_json(os.path.join(out, "03-screenshot.json"), env_shot)
                    if ok_shot and os.path.isfile(png):
                        report["screenshot"] = {
                            "file": "screen.png",
                            "bytes": os.path.getsize(png),
                            "sha256": sha256(png),
                            "frame": (env_shot.get("result") or {}).get("frame"),
                        }
                except TimeoutError as e:
                    report["steps"].append(
                        {"step": "screenshot", "wall_ms": args.step_timeout * 1000,
                         "ok": False, "status": "timeout",
                         "error": {"code": "HARNESS_TIMEOUT", "message": str(e)}}
                    )
            report["stages"]["screenshot"] = {"ok": ok_shot}

            # ---- tap the first clickable node ----------------------------
            node, bounds = first_clickable(tree_nodes)
            ok_tap = False
            if ok_tree and node is not None:
                x = (bounds[0] + bounds[2]) // 2
                y = (bounds[1] + bounds[3]) // 2
                try:
                    env_tap, wall_p, rid = client.call(
                        "acos_tap", {"x": x, "y": y}, args.step_timeout
                    )
                    ok_tap = bool(env_tap.get("ok"))
                    tap_result = (env_tap.get("result") or {})
                    report["tap"] = {
                        "x": x,
                        "y": y,
                        "bounds": node.get("bounds"),
                        "class": node.get("class"),
                        "resource_id": node.get("resource_id"),
                        "text": node.get("text"),
                        "accepted": tap_result.get("accepted"),
                        "state_changed": tap_result.get("state_changed"),
                        "settle": tap_result.get("settle"),
                    }
                    report["steps"].append(
                        {"step": "tap", "request_id": rid, "wall_ms": wall_p,
                         "ok": ok_tap, "status": env_tap.get("status"),
                         "error": error_of(env_tap)}
                    )
                    write_json(os.path.join(out, "04-tap.json"), env_tap)
                except TimeoutError as e:
                    report["steps"].append(
                        {"step": "tap", "wall_ms": args.step_timeout * 1000, "ok": False,
                         "status": "timeout", "error": {"code": "HARNESS_TIMEOUT", "message": str(e)}}
                    )
                # ---- tree after tap --------------------------------------
                ok_tree2 = False
                if ok_tap:
                    try:
                        env_tree2, wall_t2, rid = client.call(
                            "acos_observe_tree", {}, args.step_timeout
                        )
                        ok_tree2 = bool(env_tree2.get("ok"))
                        report["steps"].append(
                            {"step": "tree_after", "request_id": rid, "wall_ms": wall_t2,
                             "ok": ok_tree2, "status": env_tree2.get("status"),
                             "error": error_of(env_tree2)}
                        )
                        write_json(os.path.join(out, "05-tree-after.json"), env_tree2)
                        if ok_tree2:
                            report["texts_after"] = texts(
                                ((env_tree2.get("result") or {}).get("tree") or {})
                            )
                    except TimeoutError as e:
                        report["steps"].append(
                            {"step": "tree_after", "wall_ms": args.step_timeout * 1000,
                             "ok": False, "status": "timeout",
                             "error": {"code": "HARNESS_TIMEOUT", "message": str(e)}}
                        )
                report["stages"]["tap"] = {
                    "ok": ok_tap,
                    "tree_after_ok": ok_tree2,
                    "clickable": node is not None,
                }
            else:
                report["stages"]["tap"] = {
                    "ok": False,
                    "clickable": node is not None,
                    "reason": "no clickable node in the tree" if ok_tree else "no tree",
                }
            report["ok"] = bool(
                report["stages"].get("first_frame", {}).get("ok")
                and ok_tree
                and ok_shot
                and ok_tap
            )
        else:
            # Launch failed: classify the rung from the error, the ART markers
            # and the stage log, never from a guess.
            err = report["steps"][-1].get("error") or {}
            if err:
                report["errors"].append({"step": "launch", **err})
            classify_failure(report, err, out)
            try:
                env_status, wall_st, rid = client.call("acos_status", {}, 10)
                write_json(os.path.join(out, "01b-status.json"), env_status)
                report["steps"].append(
                    {"step": "status", "request_id": rid, "wall_ms": wall_st,
                     "ok": bool(env_status.get("ok")), "status": env_status.get("status"),
                     "error": error_of(env_status)}
                )
            except (TimeoutError, RuntimeError, OSError):
                pass

        # ---- destroy (best effort) ---------------------------------------
        try:
            env_d, wall_d, rid = client.call("acos_destroy", {}, 15)
            report["steps"].append(
                {"step": "destroy", "request_id": rid, "wall_ms": wall_d,
                 "ok": bool(env_d.get("ok")), "status": env_d.get("status"),
                 "error": error_of(env_d)}
            )
        except (TimeoutError, RuntimeError, OSError):
            pass
    finally:
        sampler.stop.set()
        time.sleep(0.15)
        report["peak_rss_mib"] = round(sampler.peak_kb / 1024.0, 1)
        try:
            if client:
                client.sock.close()
        except OSError:
            pass
        serve.send_signal(signal.SIGTERM)
        try:
            serve.wait(timeout=20)
        except subprocess.TimeoutExpired:
            serve.kill()
            serve.wait(timeout=10)
        serve_log.close()
        try:
            os.unlink(sock_path)
        except FileNotFoundError:
            pass

    # ---- ART stage markers / stage log ----------------------------------
    frames_dir = os.path.join(artifacts, "k1", "art-frames")
    for name in (
        "application-created.marker",
        "activity-resumed.marker",
        "activity-not-resumed.marker",
        "launch.json",
    ):
        p = os.path.join(frames_dir, name)
        if os.path.isfile(p):
            try:
                with open(p, errors="replace") as f:
                    report["markers"][name] = f.read()[:4000]
            except OSError:
                pass
    up = os.path.join(frames_dir, "unsupported.jsonl")
    if os.path.isfile(up):
        with open(up, errors="replace") as f:
            report["markers"]["unsupported.jsonl"] = f.read()[:8000]
    report["stages_log"] = stage_lines(os.path.join(out, "serve.log"))
    if report.get("crash_class") is None:
        report["crash_class"] = crash_class_from(report)
    write_report(report, args, out)
    log(
        f"{args.name}: ok={report['ok']} stages="
        f"{ {k: v.get('ok') for k, v in report['stages'].items()} }"
        f" first_frame_ms={report['time_to_first_frame_ms']} peak={report['peak_rss_mib']}MiB"
    )
    return 0


def write_json(path, obj):
    with open(path, "w") as f:
        json.dump(obj, f, indent=1)
        f.write("\n")


def stage_lines(path):
    out = []
    try:
        with open(path, errors="replace") as f:
            for line in f:
                if "acos-art: stage " in line or "acos-art: launch:" in line:
                    out.append(line.strip()[:400])
    except OSError:
        pass
    return out[-80:]


def classify_failure(report, err, out):
    """Fill the ladder rungs from evidence: markers, structured error, stage log."""
    code = (err or {}).get("code")
    markers = [n for n in ("application-created.marker", "activity-resumed.marker")
               if os.path.isfile(os.path.join(out, "art-sessions", "k1", "art-frames", n))]
    rung = "process_start"
    if "application-created.marker" in markers:
        rung = "application_on_create"
    if "activity-resumed.marker" in markers:
        rung = "activity_resumed"
    stage = (err or {}).get("stage")
    if stage:
        report["stages"]["engine_stage"] = stage
    report["markers_present"] = markers
    report["stages"]["process_start"] = {
        "ok": rung != "process_start",
        "rung": rung,
        "reason": None if rung != "process_start" else "no Application.onCreate marker",
    }
    report["stages"]["application_on_create"] = {
        "ok": rung in ("application_on_create", "activity_resumed"),
        "reason": "activity marker present" if rung == "activity_resumed"
        else ("application-created marker present" if rung == "application_on_create"
              else "no application-created marker"),
    }
    report["stages"]["first_frame"] = {"ok": False}
    report["stages"]["tree"] = {"ok": False}
    report["stages"]["screenshot"] = {"ok": False}
    report["stages"]["tap"] = {"ok": False}
    report["crash_class"] = crash_class_from(report)


def crash_class_from(report):
    codes = [e.get("code") for e in report.get("errors") or [] if e.get("code")]
    sig = next((e.get("guest_signal") for e in report.get("errors") or []
                if e.get("guest_signal")), None)
    if sig:
        return f"guest-signal-{sig}"
    if any(c and ("CRASH" in c or c in ("GUEST_EXITED", "GUEST_EXIT_AFTER_RESUME",
                                        "NO_ACTIVITY_RESUMED")) for c in codes):
        return "guest-exit"
    if any(c and "TIMEOUT" in c for c in codes):
        return "timeout"
    if any(c and c.startswith("UNSUPPORTED") for c in codes):
        return "engine-unsupported"
    if codes:
        return "error"
    return None


def write_report(report, args, out):
    report["finished_at"] = time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime())
    path = args.json or os.path.join(out, "report.json")
    write_json(path, report)


if __name__ == "__main__":
    sys.exit(main())
