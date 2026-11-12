#!/usr/bin/env python3
"""ZM1 driver: per-category CoW accounting of a zygote child over a session.

Reuses the Z4 protocol and seams verbatim (`ACOS_ZYGOTE_SESSION` / one zygote
forking one child per session, each child re-entering the unmodified
`LaunchMain.main`), and adds:

  * a full `/proc/PID/smaps` snapshot of every guest child (raw gzip kept) and
    of the zygote parent at each checkpoint, aggregated per mapping category
    by `smaps.py`;
  * real session use between checkpoints (10 and 100 taps, each read back);
  * an explicit GC-on-command at checkpoint 4, through a research-only watcher
    thread in the child (`<sessions>/<k>/gc` -> `gc.done`); the normal path has
    no such hook, so its checkpoint 4 is "after whatever GCs happened";
  * an optional ~3 minute idle window (JIT, profile saver, background GC);
  * an optional dialog checkpoint (`--dialog`, with the dialog-menu fixture).

Nothing here changes production code: the child-side hooks live only in this
directory's research `ZygoteApp`.
"""
import argparse
import gzip
import json
import os
import re
import subprocess
import sys
import time

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
import smaps as sm  # noqa: E402

REPO = os.path.abspath(os.path.join(HERE, "..", "..", ".."))
CACHE = os.environ.get("ACOS_CACHE", "/home/Qui/Projects/acos/.cache")
APK_COUNTER = os.path.join(REPO, "fixtures/build/apps/counter.apk")
APK_DIALOG = os.path.join(REPO, "fixtures/build/apps/dialog-menu.apk")
ART = os.environ.get("ACOS_ART_BIN", os.path.join(CACHE, "zm1-target/release/acos-art"))


# ---- protocol helpers (same shapes as research/zygote-proto/z4/drive.py) ----

def find_nodes(node, out, pred):
    if not isinstance(node, dict):
        return
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
    m = re.match(r"\[(-?\d+),(-?\d+)\]\[(-?\d+),(-?\d+)\]", node["bounds"])
    x1, y1, x2, y2 = (int(g) for g in m.groups())
    return (x1 + x2) // 2, (y1 + y2) // 2


class Timeout(Exception):
    pass


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
                chunk = os.read(self.proc.stdout.fileno(), 1 << 16)
            if not chunk:
                raise Timeout("launcher stdout closed before a reply")
            self.buf += chunk
            if time.time() > deadline:
                raise Timeout("timed out waiting for a reply")
        line, self.buf = self.buf.split(b"\n", 1)
        return line.decode("utf-8", "replace")

    def call(self, obj):
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


def launch_env(mode, session_dir=None, plan=None, n=None, zygote_build=None,
               out_dir=None, tag=None):
    env = dict(os.environ)
    env.update({
        "ACOSLAUNCH_INTERACTIVE": "1",
        "ACOSLAUNCH_LIVE_PROBE": "1",
        "ACOSLAUNCH_FRAME_WAIT_SECS": "30",
        "ACOS_INPUT_ACK_MS": "2500",
        "ACOS_FONT_DIR": "/home/Qui/Projects/acos/.cache/boot/android-34/fonts",
    })
    for k in ("ACOSLAUNCH_HOLD_MS", "ACOS_ZYGOTE_APP", "ACOS_ZYGOTE_SESSION",
              "ACOS_ZYGOTE_PLAN_DIR", "ACOS_ZYGOTE_N"):
        env.pop(k, None)
    if mode == "zygote":
        env["ACOS_ZYGOTE_SESSION"] = session_dir
        if plan is not None:
            env["ACOS_ZYGOTE_PLAN_DIR"] = plan
            env["ACOS_ZYGOTE_N"] = str(n)
        if zygote_build is not None:
            env["ACOS_ZYGOTE_APP"] = zygote_build
    env["ACOSLAUNCH_GUEST_STDOUT_FILE"] = os.path.join(out_dir, f"guest-{tag}.out")
    env["ACOSLAUNCH_GUEST_STDERR_FILE"] = os.path.join(out_dir, f"guest-{tag}.err")
    return env


def spawn_launcher(apk, mode, tag, out_dir, session_dir=None, plan=None, n=None,
                   zygote_build=None, timeout=900):
    os.makedirs(out_dir, exist_ok=True)
    cmd = [ART, "launch", "--apk", apk, "--until", "activity-resumed",
           "--out", out_dir, "--timeout", str(timeout)]
    env = launch_env(mode, session_dir, plan, n, zygote_build, out_dir, tag)
    err = open(os.path.join(out_dir, f"launcher-{tag}.err"), "wb")
    return subprocess.Popen(cmd, cwd=REPO, env=env, stdin=subprocess.PIPE,
                            stdout=subprocess.PIPE, stderr=err, bufsize=0)


def write_binds(plan, tag):
    s = os.path.join(plan, str(tag))
    rows = [
        (f"/sessions/{tag}/run/dalvik-cache", "/data/dalvik-cache"),
        (f"/sessions/{tag}/run/misc", "/data/misc"),
        (f"/sessions/{tag}/run/tmp", "/data/tmp"),
        (f"/sessions/{tag}/run/out", "/data/out"),
        (f"/sessions/{tag}/run/task", "/data/task"),
        (f"/sessions/{tag}/run/app-data", "/data/app-data"),
        (f"/sessions/{tag}/run/app-lib", "/data/app-lib"),
        (f"/sessions/{tag}/sock", "/dev/socket"),
        (f"/sessions/{tag}/sock/peer.sock", "/data/acos-launch.sock"),
    ]
    with open(os.path.join(s, "binds"), "w") as f:
        for a, b in rows:
            f.write(f"{a}\t{b}\n")


def wait_file(path, timeout=120.0):
    deadline = time.time() + timeout
    while time.time() < deadline:
        if os.path.exists(path):
            return True
        time.sleep(0.05)
    return False


# ---- process sampling (same role rules as Z4) ----

def subtree(root):
    children = {}
    for name in os.listdir("/proc"):
        if not name.isdigit():
            continue
        try:
            with open(f"/proc/{name}/stat", "rb") as f:
                line = f.read().decode("utf-8", "replace")
        except OSError:
            continue
        rest = line[line.rfind(")") + 2:].split()
        children.setdefault(int(rest[1]), []).append(int(name))
    out, todo = [], [root]
    while todo:
        pid = todo.pop()
        out.append(pid)
        todo.extend(children.get(pid, []))
    return out


def classify(pid):
    # The forked app child keeps the zygote's argv (`... ZygoteApp ...`), so the
    # comm is the only reliable role signal; check it before the cmdline.
    try:
        comm = open(f"/proc/{pid}/comm").read().strip()
        cmd = open(f"/proc/{pid}/cmdline", "rb").read().decode("utf-8", "replace")
    except OSError:
        return "gone", ""
    if comm.startswith("fixtures") or comm.startswith("dev.acos"):
        return "guest", comm
    if comm in ("z4ns", "z4init", "z4app"):
        return "sandbox", comm
    if comm == "bwrap":
        return "bwrap", comm
    if comm in ("sleep", "toybox") or "system/bin/sleep" in cmd:
        return "stub", comm
    if "ZygoteApp" in cmd:
        return "zygote", comm
    if "dalvikvm64" in cmd:
        return "guest", comm
    if comm == "acos-art" or "acos-art" in cmd:
        return "launcher", comm
    if comm.startswith("python"):
        return "driver", comm
    return "other", comm


def read_rollup(pid):
    roll = {}
    try:
        with open(f"/proc/{pid}/smaps_rollup") as f:
            for line in f:
                if ":" in line:
                    k, v = line.split(":", 1)
                    parts = v.split()
                    roll[k.strip()] = int(parts[0]) if parts else 0
    except OSError:
        return None
    return roll


def rollout(pid):
    roll = read_rollup(pid)
    if roll is None:
        return None
    role, comm = classify(pid)
    return {"pid": pid, "role": role, "comm": comm,
            "rss": roll.get("Rss", 0), "pss": roll.get("Pss", 0),
            "pss_anon": roll.get("Pss_Anon", 0), "pss_file": roll.get("Pss_File", 0),
            "pss_shmem": roll.get("Pss_Shmem", 0),
            "private_dirty": roll.get("Private_Dirty", 0),
            "private_clean": roll.get("Private_Clean", 0),
            "shared_dirty": roll.get("Shared_Dirty", 0),
            "shared_clean": roll.get("Shared_Clean", 0)}


SAMPLE_FIELDS = ("rss", "pss", "private_dirty", "private_clean",
                 "shared_dirty", "shared_clean")


def sample_checkpoint(label, root, report, rawdir, extra=None):
    t0 = time.time()
    rows = [r for r in (rollout(p) for p in sorted(subtree(os.getpid()))) if r]
    rec = {"label": label,
           "at_utc": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
           "processes": rows}
    if extra:
        rec.update(extra)
    for role in ("zygote", "guest"):
        procs = [r for r in rows if r["role"] == role]
        if not procs:
            continue
        rec[role + "_pids"] = [r["pid"] for r in procs]
    for role in ("zygote", "guest"):
        for r in [x for x in rows if x["role"] == role]:
            pid = r["pid"]
            try:
                maps, roll = sm.parse(f"/proc/{pid}/smaps")
            except OSError:
                continue
            cats = sm.aggregate(maps)
            rec.setdefault(role + "_smaps", {})[str(pid)] = {
                "rollup": roll,
                "categories": sm.to_mib(cats),
                "pss_mib": sm.pss_mib(cats),
                "top": sm.top_names(maps, 10),
            }
            if rawdir:
                os.makedirs(rawdir, exist_ok=True)
                with gzip.open(os.path.join(rawdir, f"{role}-{pid}-{label}.smaps.gz"), "wt") as f:
                    f.write(open(f"/proc/{pid}/smaps", "r", errors="replace").read())
    sums = {}
    for r in rows:
        s = sums.setdefault(r["role"], {k: 0 for k in SAMPLE_FIELDS})
        s["n"] = s.get("n", 0) + 1
        for k in SAMPLE_FIELDS:
            s[k] += r[k]
    rec["sum_by_role"] = sums
    rec["guest_side_mib"] = {k: round(sum(r[k] for r in rows
                                          if r["role"] in ("zygote", "guest", "sandbox")) / 1024.0, 2)
                             for k in SAMPLE_FIELDS}
    rec["whole_mib"] = {k: round(sum(r[k] for r in rows if r["role"] != "driver") / 1024.0, 2)
                        for k in SAMPLE_FIELDS}
    rec["sample_ms"] = round((time.time() - t0) * 1000.0, 1)
    report.setdefault("checkpoints", {})[label] = rec
    print(f"[cp {label}] guest_side pss={rec['guest_side_mib']['pss']} "
          f"pd={rec['guest_side_mib']['private_dirty']} "
          f"sd={rec['guest_side_mib']['shared_dirty']} "
          f"({rec['sample_ms']:.0f} ms)", flush=True)
    return rec


def tap(sess, pt, rec, tag):
    x, y = pt
    try:
        reply, _, ms = sess.call({"cmd": "tap", "x": x, "y": y})
    except Timeout as e:
        # The app's software draw path can stall in Surface.nativeLockCanvas
        # when taps outrun the host compositor's buffer release. Stop tapping
        # this session, keep sampling memory (the process is still there).
        rec["tap_timeout"] = str(e)
        return False
    ok = bool(reply.get("ok"))
    rec["tap_ms"] = round(ms, 2)
    if not ok:
        rec.setdefault("tap_failures", 0)
        rec["tap_failures"] += 1
    return ok


def count_text(sess, rec, name):
    try:
        tree, _, _ = sess.call({"cmd": "tree"})
    except Timeout as e:
        rec["tree_timeout"] = str(e)
        return None
    if name:
        rec[name] = tree
    nodes = []
    find_nodes(tree.get("tree") or {}, nodes,
               lambda n: str(n.get("id", "")).endswith(":id/count"))
    return nodes[0].get("text") if nodes else None


def request_gc(plan, tag, timeout=30.0):
    req = os.path.join(plan, str(tag), "gc")
    done = os.path.join(plan, str(tag), "gc.done")
    try:
        os.unlink(done)
    except OSError:
        pass
    open(req, "w").close()
    deadline = time.time() + timeout
    while time.time() < deadline:
        if os.path.exists(done):
            return open(done).read().strip()
        time.sleep(0.05)
    return None


def run(args):
    if os.getenv("ACOSLAUNCH_PROBE") is None:
        os.environ["ACOSLAUNCH_PROBE"] = ""  # forwarded to the guest by launch.rs
    apk = APK_DIALOG if args.dialog else APK_COUNTER
    root = os.path.abspath(args.out)
    os.makedirs(root, exist_ok=True)
    plan = os.path.join(root, "sessions")
    raw = os.path.join(root, "raw")
    if args.mode == "zygote":
        for k in range(args.n):
            os.makedirs(os.path.join(plan, str(k), "run"), exist_ok=True)
            os.makedirs(os.path.join(plan, str(k), "sock"), exist_ok=True)

    report = {"_t0": time.time(), "mode": args.mode, "n": args.n, "apk": os.path.basename(apk),
              "dialog": bool(args.dialog), "taps": args.taps,
              "idle_secs": args.idle_secs, "sessions": {}, "failures": [],
              "started_utc": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime())}
    procs, sessions = {}, {}

    def start(tag, zygote=False):
        out_dir = os.path.join(root, f"s{tag}")
        procs[tag] = spawn_launcher(
            apk, args.mode, tag, out_dir,
            session_dir=os.path.join(plan, str(tag)) if args.mode == "zygote" else None,
            plan=plan if zygote else None, n=args.n,
            zygote_build=args.zygote if zygote else None)
        sessions[tag] = Session(procs[tag], min(args.timeout, 45.0))

    t_start = time.perf_counter()
    if args.mode == "normal":
        for k in range(args.n):
            start(k)
    else:
        for k in range(1, args.n):
            start(k)
            wait_file(os.path.join(plan, str(k), "argv"))
            write_binds(plan, k)
        start(0, zygote=True)
        wait_file(os.path.join(plan, "0", "argv"))
        write_binds(plan, 0)
    report["spawn_ms"] = round((time.perf_counter() - t_start) * 1000.0, 1)

    for k in range(args.n):
        sess = sessions[k]
        ready = None
        deadline = time.time() + args.timeout
        while time.time() < deadline:
            rawln = sess.line().strip()
            if rawln.startswith("{") and '"ready"' in rawln:
                ready = json.loads(rawln)
                break
        rec = {"tag": k, "ready": ready}
        report["sessions"][str(k)] = rec
        if not ready or not ready.get("ok"):
            report["failures"].append(f"session {k}: not ready: {ready}")
            return finish(procs, sessions, report, args)
        tree, _, _ = sess.call({"cmd": "tree"})
        nodes = []
        find_nodes(tree.get("tree") or {}, nodes,
                   lambda n: str(n.get("id", "")).endswith(":id/inc"))
        if not nodes:
            report["failures"].append(f"session {k}: no :id/inc node")
            return finish(procs, sessions, report, args)
        rec["inc_center"] = list(center(nodes[0]))
        if args.dialog:
            db = by_id(tree.get("tree") or {}, ":id/dialog")
            if db is None:
                report["failures"].append(f"session {k}: no :id/dialog node")
                return finish(procs, sessions, report, args)
            rec["dialog_center"] = list(center(db))
        print(f"[{k}] ready frame={ready.get('frame_seq')} inc={rec['inc_center']}", flush=True)

    # checkpoint 1: right after fork + first frame
    sample_checkpoint("1_after_first_frame", root, report, os.path.join(raw, "cp1"))

    if args.dialog:
        # checkpoint D1 (base) is taken above; open the dialog and sample.
        for k in range(args.n):
            tap(sessions[k], report["sessions"][str(k)]["dialog_center"],
                report["sessions"][str(k)], k)
        time.sleep(1.0)
        for k in range(args.n):
            report["sessions"][str(k)]["tree_after_dialog"] = sessions[k].call({"cmd": "tree"})[0]
        sample_checkpoint("D2_dialog_open", root, report, os.path.join(raw, "cpD2"))
        return finish(procs, sessions, report, args)

    # checkpoint 2: after 10 taps; checkpoint 3: after up to `--taps` taps,
    # bounded by `--max-tap-secs` (the tap protocol waits for a settled frame,
    # and the software draw path stalls if taps outrun the compositor).
    taps_done = {k: 0 for k in range(args.n)}
    stalled = set()
    tap_deadline = time.time() + args.max_tap_secs
    for i in range(1, args.taps + 1):
        for k in range(args.n):
            if k in stalled:
                continue
            if not tap(sessions[k], report["sessions"][str(k)]["inc_center"],
                       report["sessions"][str(k)], k):
                if report["sessions"][str(k)].get("tap_timeout"):
                    stalled.add(k)
                continue
            taps_done[k] += 1
            time.sleep(args.tap_gap)
        if i == 10:
            for k in range(args.n):
                report["sessions"][str(k)]["count_after_10"] = count_text(
                    sessions[k], report["sessions"][str(k)], None)
            time.sleep(0.3)
            sample_checkpoint("2_after_10_taps", root, report, os.path.join(raw, "cp2"))
        if time.time() > tap_deadline or len(stalled) == args.n:
            report["tap_budget_hit"] = True
            break
    report["taps_done"] = {str(k): v for k, v in taps_done.items()}
    report["tap_stalled_sessions"] = sorted(stalled)
    for k in range(args.n):
        report["sessions"][str(k)]["count_after_taps"] = count_text(
            sessions[k], report["sessions"][str(k)], "tree_after_taps")
    time.sleep(0.3)
    sample_checkpoint("3_after_100_taps" if min(taps_done.values()) >= args.taps
                      else f"3_after_{min(taps_done.values())}_taps",
                      root, report, os.path.join(raw, "cp3"))

    # checkpoint 4: explicit GC where the path allows it
    gc = {}
    for k in range(args.n):
        if args.mode == "zygote":
            gc[str(k)] = request_gc(plan, k)
        else:
            gc[str(k)] = None
    report["gc"] = gc
    time.sleep(1.0)
    sample_checkpoint("4_after_gc", root, report, os.path.join(raw, "cp4"))

    # checkpoint 5: after ~3 minutes idle (JIT, profile saver, background GC)
    print(f"[idle] {args.idle_secs}s", flush=True)
    time.sleep(args.idle_secs)
    sample_checkpoint("5_after_idle", root, report, os.path.join(raw, "cp5"))
    for k in range(args.n):
        report["sessions"][str(k)]["count_after_idle"] = count_text(
            sessions[k], report["sessions"][str(k)], None)
    return finish(procs, sessions, report, args)


def finish(procs, sessions, report, args):
    for k, sess in sessions.items():
        try:
            reply, _, _ = sess.call({"cmd": "close"})
            report["sessions"].setdefault(str(k), {})["close"] = {"ok": bool(reply.get("ok"))}
        except (SystemExit, Timeout) as e:
            report["sessions"].setdefault(str(k), {})["close"] = {"error": str(e)}
    exits = {}
    for k, p in procs.items():
        try:
            p.stdin.close()
        except Exception:
            pass
        try:
            exits[str(k)] = p.wait(timeout=90)
        except subprocess.TimeoutExpired:
            p.kill()
            exits[str(k)] = p.wait()
    report["launcher_exit"] = exits
    report["total_ms"] = round((time.time() - report.pop("_t0", time.time())) * 1000.0, 1)
    report["verdict"] = "pass" if not report["failures"] else "fail"
    with open(args.json, "w") as f:
        json.dump(report, f, indent=1, sort_keys=True)
    print(f"[{args.mode}-n{args.n}{'-dialog' if args.dialog else ''}] "
          f"verdict={report['verdict']} failures={report['failures']} -> {args.json}",
          flush=True)
    sys.exit(0 if report["verdict"] == "pass" else 1)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--mode", required=True, choices=["normal", "zygote"])
    ap.add_argument("--n", type=int, required=True)
    ap.add_argument("--out", required=True)
    ap.add_argument("--json", required=True)
    ap.add_argument("--zygote", default=os.path.join(CACHE, "zm1-build"))
    ap.add_argument("--taps", type=int, default=100)
    ap.add_argument("--tap-gap", type=float, default=0.5)
    ap.add_argument("--max-tap-secs", type=float, default=150.0)
    ap.add_argument("--idle-secs", type=float, default=180.0)
    ap.add_argument("--dialog", action="store_true")
    ap.add_argument("--timeout", type=float, default=600.0)
    args = ap.parse_args()
    run(args)


if __name__ == "__main__":
    main()
