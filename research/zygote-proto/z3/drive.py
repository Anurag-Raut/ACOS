#!/usr/bin/env python3
"""Z3 driver: one `acos-art launch --interactive` session, tree + tap + screenshots.

Runs the *product* interactive protocol (line-JSON on the launcher's stdin, one
reply line per command on its stdout) against either path:

    --zygote <dir>   ACOS_ZYGOTE_APP=<dir>: the guest is the research zygote
                     (`dev.acos.art.ZygoteApp`) and the app runs in a child
                     forked from it
    (no --zygote)    the normal exec path, byte-for-byte the production one

Steps, all on the strict settled frame (the launcher's own
`wait_for_settled_observation`, not a fixed sleep):

    tree        -> count text must be "0"; the :id/inc bounds come from the tree
    screenshot  -> PNG A (macOS-free: the C1 stored buffer, md5 from the host)
    tap(inc)    -> real InputChannel tap; ack alone is not success
    tree        -> count text must be "1"
    screenshot  -> PNG B

Writes one JSON report (`--json`) with the texts, bounds, frame seqs, md5s and
PNG paths; the cross-path comparison is `compare.py`.
"""
import argparse
import json
import os
import subprocess
import sys
import time

ROOT = "/home/Qui/.paseo/worktrees/3oxa6xao/zygote-z3"


def find_nodes(node, out, pred):
    """The in-guest probe emits `{"nodes":[…]}`; each node has `children`."""
    if "nodes" in node and isinstance(node["nodes"], list):
        for n in node["nodes"]:
            find_nodes(n, out, pred)
        return
    if pred(node):
        out.append(node)
    for c in node.get("children") or []:
        find_nodes(c, out, pred)


def by_id(tree, suffix):
    found = []
    find_nodes(tree, found, lambda n: str(n.get("id", "")).endswith(suffix))
    return found[0] if found else None


def center(node):
    import re

    m = re.match(r"\[(-?\d+),(-?\d+)\]\[(-?\d+),(-?\d+)\]", node["bounds"])
    x1, y1, x2, y2 = (int(g) for g in m.groups())
    return (x1 + x2) // 2, (y1 + y2) // 2


class Session:
    def __init__(self, proc, timeout):
        self.proc = proc
        self.buf = b""
        self.timeout = timeout

    def line(self):
        deadline = time.time() + self.timeout
        while b"\n" not in self.buf:
            try:
                chunk = self.proc.stdout.read1(1 << 16)
            except AttributeError:
                chunk = self.proc.stdout.read(1 << 16)
            if not chunk:
                raise SystemExit("launcher stdout closed before a reply")
            self.buf += chunk
            if time.time() > deadline:
                raise SystemExit("timed out waiting for a reply")
        line, self.buf = self.buf.split(b"\n", 1)
        return line.decode("utf-8", "replace")

    def call(self, obj):
        """Send one command and return (reply, skipped_guest_lines, ms)."""
        t0 = time.perf_counter()
        self.proc.stdin.write((json.dumps(obj) + "\n").encode())
        self.proc.stdin.flush()
        skipped = []
        while True:
            raw = self.line().strip()
            if not raw:
                continue
            if raw.startswith("{"):
                try:
                    reply = json.loads(raw)
                except json.JSONDecodeError:
                    skipped.append(raw)
                    continue
                if reply.get("cmd") == obj["cmd"] or reply.get("cmd") == "session":
                    return reply, skipped, (time.perf_counter() - t0) * 1000.0
                skipped.append(raw)
            else:
                skipped.append(raw)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--mode", required=True, help="label, e.g. normal | zygote")
    ap.add_argument("--zygote", default=None, help="ACOS_ZYGOTE_APP build dir")
    ap.add_argument("--apk", default=f"{ROOT}/fixtures/build/apps/counter.apk")
    ap.add_argument("--out", required=True, help="session output dir")
    ap.add_argument("--json", required=True)
    ap.add_argument("--timeout", type=float, default=240.0)
    ap.add_argument("--sample", default=None, help="write smaps_rollup of the process tree here")
    args = ap.parse_args()

    os.makedirs(args.out, exist_ok=True)
    env = dict(os.environ)
    env.update(
        {
            "ACOSLAUNCH_INTERACTIVE": "1",
            "ACOSLAUNCH_LIVE_PROBE": "1",
            "ACOSLAUNCH_FRAME_WAIT_SECS": "30",
            "ACOS_INPUT_ACK_MS": "2500",
            "ACOS_FONT_DIR": "/home/Qui/Projects/acos/.cache/boot/android-34/fonts",
        }
    )
    env.pop("ACOSLAUNCH_HOLD_MS", None)
    env.pop("ACOS_ZYGOTE_APP", None)
    if args.zygote:
        env["ACOS_ZYGOTE_APP"] = args.zygote
    launcher_err = os.path.join(args.out, "launcher.err")
    env["ACOSLAUNCH_GUEST_STDOUT_FILE"] = os.path.join(args.out, "guest.out")
    env["ACOSLAUNCH_GUEST_STDERR_FILE"] = os.path.join(args.out, "guest.err")
    cmd = [
        os.environ.get(
            "ACOS_ART_BIN", "/home/Qui/Projects/acos/.cache/z3-target/release/acos-art"
        ),
        "launch",
        "--apk",
        args.apk,
        "--until",
        "activity-resumed",
        "--out",
        args.out,
        "--timeout",
        "300",
    ]
    started = time.perf_counter()
    proc = subprocess.Popen(
        cmd,
        cwd=ROOT,
        env=env,
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=open(launcher_err, "wb"),
        bufsize=0,
    )
    sess = Session(proc, args.timeout)
    report = {
        "mode": args.mode,
        "zygote": args.zygote,
        "cmd": cmd,
        "started_utc": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
        "steps": [],
        "failures": [],
    }

    def step(name, req, extra=None):
        reply, skipped, ms = sess.call(req)
        rec = {
            "step": name,
            "request": req,
            "ok": bool(reply.get("ok")),
            "cmd": reply.get("cmd"),
            "frame_seq": reply.get("frame_seq"),
            "md5": reply.get("md5"),
            "png": reply.get("png"),
            "buffer_generation": reply.get("buffer_generation"),
            "frame_number": reply.get("frame_number"),
            "pixels_fnv1a": reply.get("pixels_fnv1a"),
            "non_zero_pixels": reply.get("non_zero_pixels"),
            "wall_ms": round(ms, 3),
            "guest_lines": skipped[-40:],
        }
        if extra:
            rec.update(extra)
        if "tree" in reply and isinstance(reply["tree"], dict):
            texts = []
            find_nodes(reply["tree"], texts, lambda n: bool(n.get("text")))
            rec["texts"] = [n["text"] for n in texts]
            rec["tree"] = reply["tree"]
        if "error" in reply:
            rec["error"] = reply["error"]
        for k in ("code", "tap"):
            if k in reply:
                rec[k] = reply[k]
        report["steps"].append(rec)
        print(f"[{args.mode}] {name}: ok={rec['ok']} seq={rec['frame_seq']} "
              f"md5={rec['md5']} wall_ms={rec['wall_ms']:.1f}", flush=True)
        return reply, rec

    def fail(msg):
        report["failures"].append(msg)
        print(f"[{args.mode}] FAIL {msg}", file=sys.stderr, flush=True)

    # The ready line: the launcher prints it before it serves commands.
    ready = None
    deadline = time.time() + args.timeout
    while time.time() < deadline:
        raw = sess.line().strip()
        if raw.startswith("{") and '"ready"' in raw:
            ready = json.loads(raw)
            break
        report.setdefault("pre_ready", []).append(raw[-400:])
    report["ready"] = ready
    report["ready_ms"] = round((time.perf_counter() - started) * 1000.0, 1)
    if not ready or not ready.get("ok"):
        fail(f"launch did not reach a live frame: {ready}")
    else:
        print(f"[{args.mode}] ready in {report['ready_ms']:.0f} ms "
              f"frame_seq={ready.get('frame_seq')}", flush=True)

    def count_text(rec):
        nodes = []
        find_nodes(rec.get("tree") or {}, nodes,
                   lambda n: str(n.get("id", "")).endswith(":id/count"))
        return nodes[0].get("text") if nodes else None

    _, t0 = step("tree_initial", {"cmd": "tree"})
    got = count_text(t0)
    if got != "0":
        fail(f"tree_initial count text {got!r}, expected '0'")
    inc = by_id(t0.get("tree") or {}, ":id/inc")
    if inc is None:
        fail("no node whose id ends with ':id/inc'")
        finish(proc, report, args, sess)
        return
    xy = center(inc)
    t0["inc_bounds"] = inc["bounds"]
    t0["inc_center"] = list(xy)

    _, s0 = step("screenshot_initial", {"cmd": "screenshot"})
    if count_text(s0) is None:
        # the screenshot reply carries no tree: re-read the count from tree_initial
        pass
    tap, tap_rec = step("tap_inc", {"cmd": "tap", "x": xy[0], "y": xy[1]})
    tap_rec["inc_center"] = list(xy)
    _, t1 = step("tree_after_tap", {"cmd": "tree"})
    got = count_text(t1)
    if got != "1":
        fail(f"tree_after_tap count text {got!r}, expected '1'")
    _, s1 = step("screenshot_after_tap", {"cmd": "screenshot"})

    if args.sample:
        report["memory"] = sample_tree(proc.pid, args.sample)
    report["total_ms"] = round((time.perf_counter() - started) * 1000.0, 1)
    finish(proc, report, args, sess)


def subtree(root):
    import os as _os

    children = {}
    for name in _os.listdir("/proc"):
        if not name.isdigit():
            continue
        try:
            with open(f"/proc/{name}/stat", "rb") as f:
                line = f.read().decode("utf-8", "replace")
        except OSError:
            continue
        # comm may contain spaces/parens: parent is the field after the last ')'.
        rest = line[line.rfind(")") + 2:].split()
        children.setdefault(int(rest[1]), []).append(int(name))
    out, todo = [], [root]
    while todo:
        pid = todo.pop()
        out.append(pid)
        todo.extend(children.get(pid, []))
    return out


def sample_tree(root, path):
    """meminfo-style rollup of every process in the launcher's tree (KiB)."""
    import os as _os

    rows = []
    for pid in sorted(subtree(root)):
        try:
            comm = open(f"/proc/{pid}/comm").read().strip()
            roll = {}
            with open(f"/proc/{pid}/smaps_rollup") as f:
                for line in f:
                    if ":" in line:
                        k, v = line.split(":", 1)
                        roll[k] = int(v.split()[0]) if v.split() and v.split()[0].isdigit() else v.strip()
            groups = {}
            with open(f"/proc/{pid}/smaps") as f:
                for line in f:
                    if ":" in line and not line.startswith(" "):
                        k, v = line.split(":", 1)
                        groups.setdefault(k, 0)
                        if v.split() and v.split()[0].isdigit():
                            groups[k] += int(v.split()[0])
                    elif line[:1] in "RPS" and line.split() and line.split()[0].isdigit():
                        # per-mapping line, not a header: ignore
                        pass
        except OSError:
            continue
        rows.append({"pid": pid, "comm": comm, "rollup": roll, "groups": groups})
    with open(path, "w") as f:
        json.dump({"root": root, "at_utc": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
                   "processes": rows}, f, indent=1, sort_keys=True)
    return {"processes": len(rows), "path": path}


def finish(proc, report, args, sess=None):
    # `close` = the launcher's own clean stop (no kill).
    try:
        if sess is None:
            sess = Session(proc, 30.0)
        reply, _, ms = sess.call({"cmd": "close"})
        report["close"] = {"ok": bool(reply.get("ok")), "wall_ms": round(ms, 1)}
    except SystemExit as e:
        report["close"] = {"error": str(e)}
    try:
        proc.stdin.close()
    except Exception:
        pass
    try:
        rc = proc.wait(timeout=60)
    except subprocess.TimeoutExpired:
        proc.kill()
        rc = proc.wait()
    report["launcher_exit"] = rc
    report["verdict"] = "pass" if not report["failures"] else "fail"
    with open(args.json, "w") as f:
        json.dump(report, f, indent=1, sort_keys=True)
    print(f"[{report['mode']}] verdict={report['verdict']} "
          f"failures={len(report['failures'])} -> {args.json}", flush=True)
    sys.exit(0 if report["verdict"] == "pass" else 1)


if __name__ == "__main__":
    main()
