#!/usr/bin/env python3
"""Z4 driver: N `acos-art launch --interactive` sessions, normal path vs one
shared zygote, with the same protocol, the same checks and the same memory
sampling for both.

    --mode normal   N independent launchers, each spawns its own ART guest
    --mode zygote   N launchers in "foreign guest" mode (ACOS_ZYGOTE_SESSION):
                    each waits for a guest; ONE zygote (spawned by launcher 0)
                    pre-inits ART once and forks one child per session, each
                    child bind-mounting its own session's writable root,
                    /dev/socket and /data/acos-launch.sock in a private mount
                    namespace before re-entering the unmodified LaunchMain.

Per session: tree must read 0, a tap must give 1, every *other* not-yet-tapped
session must still read 0 (the isolation check), and both screenshots must be
byte-identical to the normal path's. Memory is sampled while all N sessions are
held after their tap, from this driver's whole process tree.
"""
import argparse
import json
import os
import re
import subprocess
import sys
import threading
import time

HERE = os.path.dirname(os.path.abspath(__file__))
# Z6: the release probe (1 Hz smaps + mapping attribution) lives next to this file
# in research/zygote-proto/z6/. Imported lazily so the Z4 harness still runs
# without that directory.
Z6_DIR = os.path.join(os.path.dirname(HERE), "z6")
if os.path.isdir(Z6_DIR) and Z6_DIR not in sys.path:
    sys.path.insert(0, Z6_DIR)
REPO = os.path.abspath(os.path.join(HERE, "..", "..", ".."))
CACHE = os.environ.get("ACOS_CACHE", "/home/Qui/Projects/acos/.cache")
APK = os.path.join(REPO, "fixtures/build/apps/counter.apk")
ART = os.environ.get("ACOS_ART_BIN", os.path.join(CACHE, "z4-target/release/acos-art"))


def find_nodes(node, out, pred):
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
                raise SystemExit("launcher stdout closed before a reply")
            self.buf += chunk
            if time.time() > deadline:
                raise SystemExit("timed out waiting for a reply")
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


CLK_TCK = os.sysconf("SC_CLK_TCK")
T0 = None


def read_cpu(pid):
    """Cumulative CPU ticks of one pid: utime+stime+cutime+cstime.

    Reaped children roll into cutime/cstime of their reaper, which is inside
    the sampled subtree, so the sum is the tree's cumulative host CPU.
    """
    try:
        with open(f"/proc/{pid}/stat", "rb") as f:
            line = f.read().decode("utf-8", "replace")
    except OSError:
        return 0
    rest = line[line.rfind(")") + 2:].split()
    try:
        return sum(int(rest[i]) for i in (11, 12, 13, 14))
    except (IndexError, ValueError):
        return 0


def cpu_snapshot(root):
    pids = subtree(root)
    ticks = sum(read_cpu(p) for p in pids)
    driver = read_cpu(root)
    return {"seconds": round(ticks / CLK_TCK, 3),
            "driver_seconds": round(driver / CLK_TCK, 3),
            "processes": len(pids)}


def launch_env(mode, session_dir=None, plan=None, n=None, zygote_build=None, out_dir=None, tag=None):
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
    for k in ("ACOSLAUNCH_HOLD_MS", "ACOS_ZYGOTE_APP", "ACOS_ZYGOTE_SESSION",
              "ACOS_ZYGOTE_PLAN_DIR", "ACOS_ZYGOTE_N"):
        env.pop(k, None)
    if mode == "zygote":
        if os.environ.get("ACOS_ZYGOTE_NO_STUB"):
            env["ACOS_ZYGOTE_NO_STUB"] = "1"
        else:
            env.pop("ACOS_ZYGOTE_NO_STUB", None)
        env["ACOS_ZYGOTE_SESSION"] = session_dir
        if plan is not None:
            env["ACOS_ZYGOTE_PLAN_DIR"] = plan
            env["ACOS_ZYGOTE_N"] = str(n)
        if zygote_build is not None:
            env["ACOS_ZYGOTE_APP"] = zygote_build
        env["ACOS_ZYGOTE_PROBE"] = "1"
    env["ACOSLAUNCH_GUEST_STDOUT_FILE"] = os.path.join(out_dir, f"guest-{tag}.out")
    env["ACOSLAUNCH_GUEST_STDERR_FILE"] = os.path.join(out_dir, f"guest-{tag}.err")
    return env


def spawn_launcher(mode, tag, out_dir, session_dir=None, plan=None, n=None, zygote_build=None):
    os.makedirs(out_dir, exist_ok=True)
    cmd = [
        ART, "launch", "--apk", APK, "--until", "activity-resumed",
        "--out", out_dir, "--timeout", "300",
    ]
    env = launch_env(mode, session_dir, plan, n, zygote_build, out_dir, tag)
    err = open(os.path.join(out_dir, f"launcher-{tag}.err"), "wb")
    return subprocess.Popen(
        cmd, cwd=REPO, env=env, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
        stderr=err, bufsize=0,
    )


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


def classify(pid):
    try:
        comm = open(f"/proc/{pid}/comm").read().strip()
        cmd = open(f"/proc/{pid}/cmdline", "rb").read().decode("utf-8", "replace")
    except OSError:
        return "gone", ""
    if "ZygoteApp" in cmd:
        return "zygote", comm
    if comm in ("z4ns", "z4init", "z4app"):
        return "sandbox", comm
    if comm == "bwrap":
        return "bwrap", comm
    if comm in ("sleep", "toybox") or "system/bin/sleep" in cmd:
        return "stub", comm
    if ("dalvikvm64" in cmd or comm.startswith(("fixtures", "ixtures", "dev.acos"))):
        return "guest", comm
    if comm == "acos-art" or "acos-art" in cmd:
        return "launcher", comm
    if comm.startswith("python"):
        return "driver", comm
    return "other", comm


def sample_memory(root, label):
    rows = []
    for pid in sorted(subtree(root)):
        roll = read_rollup(pid)
        if roll is None:
            continue
        role, comm = classify(pid)
        cg = ""
        ns = ""
        try:
            cg = open(f"/proc/{pid}/cgroup").read().strip()
        except OSError:
            pass
        try:
            with open(f"/proc/{pid}/status") as f:
                for line in f:
                    if line.startswith("NSpid:"):
                        ns = line.split(":", 1)[1].strip()
                        break
        except OSError:
            pass
        oom_group = None
        try:
            cgpath = "/sys/fs/cgroup" + cg.split("::", 1)[1]
            oom_group = int(open(cgpath + "/memory.oom.group").read().strip())
        except (OSError, IndexError, ValueError):
            pass
        rows.append({
            "pid": pid, "role": role, "comm": comm,
            "cgroup": cg, "ns_pid": ns, "oom_group": oom_group,
            "rss": roll.get("Rss", 0), "pss": roll.get("Pss", 0),
            "pss_anon": roll.get("Pss_Anon", 0),
            "pss_file": roll.get("Pss_File", 0),
            "pss_shmem": roll.get("Pss_Shmem", 0),
            "private_dirty": roll.get("Private_Dirty", 0),
            "private_clean": roll.get("Private_Clean", 0),
            "shared_dirty": roll.get("Shared_Dirty", 0),
            "shared_clean": roll.get("Shared_Clean", 0),
            # Z6: the guarded scope sets memory.swap.max=0, so both must read 0.
            "swap": roll.get("Swap", 0), "swap_pss": roll.get("SwapPss", 0),
        })
    KEYS = ("rss", "pss", "private_dirty", "shared_dirty", "shared_clean",
            "swap", "swap_pss")
    sums = {}
    for r in rows:
        s = sums.setdefault(r["role"], {"n": 0, **{k: 0 for k in KEYS}})
        s["n"] += 1
        for k in KEYS:
            s[k] += r[k]
    total = {k: sum(r[k] for r in rows) for k in KEYS}
    guest_side = {k: sum(r[k] for r in rows if r["role"] in ("zygote", "guest", "sandbox"))
                  for k in KEYS}
    # Z6: /sys/fs/cgroup evidence — the scope's swap ceiling and accounting, and
    # the memory.events counters that would show OOM kills during the sample.
    cgroups = {}
    for r in rows:
        path = r.get("cgroup") or ""
        if "::" not in path:
            continue
        base = "/sys/fs/cgroup" + path.split("::", 1)[1]
        if base in cgroups:
            continue
        ev = {}
        for name in ("memory.swap.max", "memory.swap.current", "memory.max",
                     "memory.current", "memory.oom.group", "pids.max", "pids.current"):
            try:
                ev[name] = open(os.path.join(base, name)).read().strip()
            except OSError:
                pass
        try:
            for line in open(os.path.join(base, "memory.events")):
                k, _, v = line.partition(" ")
                ev["events." + k] = v.strip()
        except OSError:
            pass
        cgroups[base] = ev
    return {"label": label, "at_utc": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
            "processes": rows, "sum_by_role": sums, "total": total, "guest_side": guest_side,
            "cgroups": cgroups}


def run_session(sess, tag, report, ready_timeout):
    try:
        _run_session(sess, tag, report, ready_timeout)
    except BaseException as e:  # never leave the main thread with a half-made dict
        import traceback
        traceback.print_exc()
        report["failures"].append(f"session {tag}: driver exception {e!r}")
        report["sessions"].setdefault(tag, {"tag": tag, "error": repr(e)})


def _run_session(sess, tag, report, ready_timeout):
    def fail(msg):
        report["failures"].append(f"session {tag}: {msg}")
        print(f"[{tag}] FAIL {msg}", file=sys.stderr, flush=True)

    started = time.perf_counter()
    ready = None
    deadline = time.time() + ready_timeout
    while time.time() < deadline:
        raw = sess.line().strip()
        if raw.startswith("{") and '"ready"' in raw:
            ready = json.loads(raw)
            break
    rec = {"tag": tag, "ready": ready,
           "ready_ms": round((time.perf_counter() - started) * 1000.0, 1),
           "t_ready_ms": round((time.perf_counter() - (T0 or started)) * 1000.0, 1),
           "steps": {}}
    report["sessions"][tag] = rec
    if not ready or not ready.get("ok"):
        fail(f"launch did not reach a live frame: {ready}")
        return
    print(f"[{tag}] ready in {rec['ready_ms']:.0f} ms frame_seq={ready.get('frame_seq')}",
          flush=True)
    tree, _, ms = sess.call({"cmd": "tree"})
    rec["steps"]["tree_initial"] = tree
    nodes = []
    find_nodes(tree.get("tree") or {}, nodes, lambda n: str(n.get("id", "")).endswith(":id/count"))
    rec["count_initial"] = nodes[0].get("text") if nodes else None
    if rec["count_initial"] != "0":
        fail(f"tree_initial count {rec['count_initial']!r}, expected '0'")
    inc = by_id(tree.get("tree") or {}, ":id/inc")
    if inc is None:
        fail("no :id/inc node")
        return
    rec["inc_bounds"] = inc["bounds"]
    rec["inc_center"] = list(center(inc))
    shot, _, _ = sess.call({"cmd": "screenshot"})
    rec["steps"]["screenshot_initial"] = shot
    rec["md5_initial"] = shot.get("md5")
    rec["frame_initial"] = shot.get("frame_number")
    rec["fnv_initial"] = shot.get("pixels_fnv1a")


def tap(sess, rec):
    x, y = rec["inc_center"]
    reply, _, ms = sess.call({"cmd": "tap", "x": x, "y": y})
    rec["steps"]["tap"] = reply
    rec["tap_ok"] = bool(reply.get("ok"))
    rec["tap_ms"] = round(ms, 2)
    return rec["tap_ok"]


def read_count(sess, rec, name):
    tree, _, ms = sess.call({"cmd": "tree"})
    rec["steps"][name] = tree
    nodes = []
    find_nodes(tree.get("tree") or {}, nodes, lambda n: str(n.get("id", "")).endswith(":id/count"))
    return nodes[0].get("text") if nodes else None


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--mode", required=True, choices=["normal", "zygote"])
    ap.add_argument("--n", type=int, required=True)
    ap.add_argument("--out", required=True)
    ap.add_argument("--json", required=True)
    ap.add_argument("--zygote", default=os.path.join(CACHE, "z4-build"))
    ap.add_argument("--timeout", type=float, default=240.0)
    ap.add_argument("--label", default="",
                    help="arm label recorded in the report (a/b/c)")
    ap.add_argument("--settle", type=float, default=0.5,
                    help="seconds to hold all N sessions after the last tap before the "
                         "memory sample (0.5 = Z4's method; 60 = D-M2's steady state)")
    ap.add_argument("--samples", default="",
                    help="Z6: comma-separated hold times (s after the last tap) at which "
                         "the whole-tree memory snapshot is taken, e.g. 0.5,15,60. Overrides "
                         "--settle; samples are cumulative in one run.")
    ap.add_argument("--release-probe", type=float, default=0.0,
                    help="Z6: seconds of 1 Hz per-process smaps sampling (with mapping "
                         "attribution and guest log correlation) right after the last "
                         "session's first frame, before the tap sequence. 0 = off.")
    args = ap.parse_args()

    root = os.path.abspath(args.out)
    os.makedirs(root, exist_ok=True)
    plan = os.path.join(root, "sessions")
    if args.mode == "zygote":
        for k in range(args.n):
            os.makedirs(os.path.join(plan, str(k), "run"), exist_ok=True)
            os.makedirs(os.path.join(plan, str(k), "sock"), exist_ok=True)

    report = {"mode": args.mode, "arm": args.label, "n": args.n, "sessions": {},
              "failures": [],
              "knobs": {k: os.environ.get(k) for k in (
                  "ACOS_SHARED_BOOT_IMAGE", "ACOS_ZYGOTE_APP", "ACOS_ZYGOTE_NO_STUB",
                  "ACOS_ART_HEAPSTARTSIZE", "ACOS_ART_HEAPGROWTHLIMIT",
                  "ACOS_ART_HEAPSIZE", "ACOS_ART_HEAPMINFREE",
                  "ACOS_ART_HEAPMAXFREE", "ACOS_ART_HEAPTARGETUTILIZATION")
                  if os.environ.get(k)},
              "started_utc": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime())}
    procs = {}
    sessions = {}

    def start(tag, zygote=False):
        out_dir = os.path.join(root, f"s{tag}")
        procs[tag] = spawn_launcher(
            args.mode, tag, out_dir,
            session_dir=os.path.join(plan, str(tag)) if args.mode == "zygote" else None,
            plan=plan if zygote else None, n=args.n,
            zygote_build=args.zygote if zygote else None,
        )
        sessions[tag] = Session(procs[tag], args.timeout)

    global T0
    t_start = time.perf_counter()
    T0 = t_start
    report["cpu_at_start"] = cpu_snapshot(os.getpid())
    if args.mode == "normal":
        for k in range(args.n):
            start(k)
    else:
        # Non-zero sessions first: they publish their contracts and wait.
        for k in range(1, args.n):
            start(k)
            wait_file(os.path.join(plan, str(k), "argv"))
            write_binds(plan, k)
        # Session 0 spawns the one zygote; it waits for all N contracts.
        start(0, zygote=True)
        wait_file(os.path.join(plan, "0", "argv"))
        write_binds(plan, 0)
    report["spawn_ms"] = round((time.perf_counter() - t_start) * 1000.0, 1)

    # Ready + initial tree/screenshot for every session. Sequential on purpose:
    # every launcher serves its own pipe, and a single reader keeps the driver
    # state simple (and its failure modes visible).
    for k in range(args.n):
        print(f"[{k}] waiting for ready", flush=True)
        run_session(sessions[k], k, report, args.timeout)
        print(f"[{k}] initial done", flush=True)
    report["ready_ms_max"] = max((s.get("ready_ms", 0) for s in report["sessions"].values()),
                                 default=0)
    # Host CPU-seconds to bring N sessions up (driver start -> every session
    # ready), both including and excluding the driver's own CPU.
    snap = cpu_snapshot(os.getpid())
    report["cpu_all_ready"] = snap
    start = report["cpu_at_start"]
    report["cpu_bringup_seconds"] = round(snap["seconds"] - start["seconds"], 3)
    report["cpu_bringup_seconds_no_driver"] = round(
        (snap["seconds"] - snap["driver_seconds"])
        - (start["seconds"] - start["driver_seconds"]), 3)
    report["all_ready_ms"] = round((time.perf_counter() - t_start) * 1000.0, 1)
    t_ready = [report["sessions"][k].get("t_ready_ms") for k in range(args.n)]
    report["sessions_ready_ms_from_spawn"] = t_ready
    if t_ready and t_ready[0] is not None and t_ready[-1] is not None:
        report["first_session_ready_ms"] = t_ready[0]
        report["per_extra_session_ready_ms"] = (
            round((t_ready[-1] - t_ready[0]) / (args.n - 1), 1) if args.n > 1 else None)
    if report["failures"]:
        finish(procs, sessions, report, args)
        return

    # Z6: the release probe — 1 Hz per-process smaps + per-mapping attribution for
    # 20 s of *idle* session time, started as soon as the last session has drawn
    # its first frame (the tap sequence below has not run yet, so nothing the
    # driver does can influence the drop).
    if args.release_probe > 0:
        import probe as z6_probe  # noqa: E402  (z6/ is on sys.path above)
        seen = {k: 0 for k in range(args.n)}
        dropped = {k: 0 for k in range(args.n)}

        def collect_logs():
            fresh = []
            for k in range(args.n):
                try:
                    reply, _, _ = sessions[k].call({"cmd": "logs"})
                except BaseException:
                    continue
                ents = reply.get("entries") or []
                drop = reply.get("dropped") or 0
                if drop != dropped[k]:
                    seen[k] = 0
                    dropped[k] = drop
                for e in ents[seen[k]:]:
                    msg = str(e.get("message", ""))
                    if re.search(r"GC freed|heap trim|Using .*GC|concurrent copying|"
                                 r"Zygote|trim", msg, re.I):
                        fresh.append({"session": k, "prio": e.get("prio"),
                                      "tag": e.get("tag"), "message": msg[:400]})
                seen[k] = len(ents)
            return fresh

        print(f"[{args.label}-n{args.n}] release probe {args.release_probe:.0f}s", flush=True)
        series = z6_probe.probe_series(os.getpid(), classify, args.release_probe,
                                       log_fn=collect_logs)
        report["release_probe"] = {
            "seconds": args.release_probe,
            "started_after": "all N sessions' first frame (before the tap sequence)",
            "series": series,
        }
        print(f"[{args.label}-n{args.n}] release probe done, {len(series)} samples", flush=True)

    # The isolation sequence: tap K, require K==1 and every *untapped* session==0.
    for k in range(args.n):
        tap(sessions[k], report["sessions"][k])
        got = read_count(sessions[k], report["sessions"][k], f"tree_after_tap_{k}")
        report["sessions"][k]["count_after_tap"] = got
        if got != "1":
            report["failures"].append(f"session {k}: count after tap {got!r}, expected '1'")
        for j in range(k + 1, args.n):
            other = read_count(sessions[j], report["sessions"][j], f"tree_isolated_after_tap_{k}")
            report["sessions"][j].setdefault("isolation_reads", {})[k] = other
            if other != "0":
                report["failures"].append(
                    f"session {j}: count {other!r} changed after session {k}'s tap")
        shot, _, _ = sessions[k].call({"cmd": "screenshot"})
        report["sessions"][k]["steps"]["screenshot_after_tap"] = shot
        report["sessions"][k]["md5_after_tap"] = shot.get("md5")
        report["sessions"][k]["frame_after_tap"] = shot.get("frame_number")
        report["sessions"][k]["fnv_after_tap"] = shot.get("pixels_fnv1a")
        print(f"[{k}] tap ok={report['sessions'][k]['tap_ok']} count={got} "
              f"md5={shot.get('md5')}", flush=True)

    # Cross-session and cross-path pixel equality.
    md5s0 = {k: report["sessions"][k].get("md5_initial") for k in range(args.n)}
    md5s1 = {k: report["sessions"][k].get("md5_after_tap") for k in range(args.n)}
    fnv0 = {k: report["sessions"][k].get("fnv_initial") for k in range(args.n)}
    fnv1 = {k: report["sessions"][k].get("fnv_after_tap") for k in range(args.n)}
    report["md5_initial_all_equal"] = len(set(md5s0.values())) == 1
    report["md5_after_tap_all_equal"] = len(set(md5s1.values())) == 1
    report["pixels_fnv1a_initial_all_equal"] = len(set(fnv0.values())) == 1
    report["pixels_fnv1a_after_tap_all_equal"] = len(set(fnv1.values())) == 1
    for name in ("md5_initial_all_equal", "md5_after_tap_all_equal",
                 "pixels_fnv1a_initial_all_equal", "pixels_fnv1a_after_tap_all_equal"):
        if not report[name]:
            report["failures"].append(name)

    # Z4SEC probes and the zygote's own fork timings, from this run's logs.
    sec = []
    fork_ms = {}
    for k in range(args.n):
        path = os.path.join(root, f"s{k}", f"guest-{k}.out")
        try:
            text = open(path, "r", errors="replace").read()
        except OSError:
            continue
        for line in text.splitlines():
            if line.startswith("Z4SEC "):
                sec.append(line)
            m = re.search(r"Z4 child tag=(\d+) fork_to_ready_ms=(\d+)", line)
            if m:
                fork_ms[int(m.group(1))] = int(m.group(2))
            m = re.search(r"Z4 child tag=(\d+) fork_to_sandbox_ms=(\d+)", line)
            if m:
                fork_ms.setdefault("sandbox", {})[int(m.group(1))] = int(m.group(2))
    report["security_probes"] = sec
    report["fork_to_ready_ms"] = {str(k): v for k, v in fork_ms.items() if isinstance(k, int)}
    report["fork_to_sandbox_ms"] = {str(k): v for k, v in (fork_ms.get("sandbox", {}) or {}).items()}

    # Held-after-tap memory snapshots of this driver's whole process tree.
    # Z6: one run yields every requested time point (0.5, 15, 60 s), so a
    # configuration's three numbers come from the same sessions and the same host
    # minute instead of three runs.
    times = sorted({float(t) for t in args.samples.split(",") if t.strip()}) \
        if args.samples else [args.settle]
    report["sample_times_s"] = times
    samples = []
    t0 = time.perf_counter()
    for t in times:
        wait = t - (time.perf_counter() - t0)
        if wait > 0:
            time.sleep(wait)
        snap = sample_memory(os.getpid(), f"{args.mode}-n{args.n}-t{t:g}")
        snap["t_s"] = t
        snap["elapsed_s"] = round(time.perf_counter() - t0, 2)
        samples.append(snap)
        print(f"[{args.label}-n{args.n}] memory t={t:g}s elapsed={snap['elapsed_s']}s "
              f"pss={snap['total']['pss'] / 1024:.1f}MiB"
              f" pd={snap['total']['private_dirty'] / 1024:.1f}MiB"
              f" swappss={snap['total']['swap_pss'] / 1024:.2f}MiB", flush=True)
    report["memory_samples"] = samples
    report["memory"] = samples[0]
    report["settle_s"] = times[0]

    # Z5 steady-state liveness: after a long settle, prove every session is still
    # interactive and still isolated *after* the memory sample (so the sample stays
    # an idle reading). A second tap must read 2 in that session only.
    if times[-1] >= 5:
        for k in range(args.n):
            tap(sessions[k], report["sessions"][k])
            got = read_count(sessions[k], report["sessions"][k], f"tree_after_settle_tap_{k}")
            report["sessions"][k]["count_after_settle_tap"] = got
            if got != "2":
                report["failures"].append(
                    f"session {k}: count after settle+tap {got!r}, expected '2'")
            for j in range(k + 1, args.n):
                other = read_count(sessions[j], report["sessions"][j],
                                   f"tree_settle_isolated_after_tap_{k}")
                if other != "1":
                    report["failures"].append(
                        f"session {j}: count {other!r} changed after a post-settle tap of {k}")
        print(f"[{args.label}-n{args.n}] post-settle liveness+isolation done", flush=True)

    report["total_ms"] = round((time.perf_counter() - t_start) * 1000.0, 1)
    finish(procs, sessions, report, args)


def finish(procs, sessions, report, args):
    # Z5: arm (c)'s `close` can hang the launcher (killing the shared zygote leaves a thread in a
    # futex wait; see Z5-gate.md §5), so teardown waits are bounded hard. All measurements are taken
    # before this point, so the shorter bound cannot change a number.
    for k, sess in sessions.items():
        sess.timeout = 20.0
        try:
            reply, _, _ = sess.call({"cmd": "close"})
            report["sessions"].setdefault(k, {})["close"] = {"ok": bool(reply.get("ok"))}
        except SystemExit as e:
            report["sessions"].setdefault(k, {})["close"] = {"error": str(e)}
    exits = {}
    for k, p in procs.items():
        try:
            p.stdin.close()
        except Exception:
            pass
        try:
            exits[k] = p.wait(timeout=20)
        except subprocess.TimeoutExpired:
            p.kill()
            exits[k] = p.wait()
    report["launcher_exit"] = exits
    report["verdict"] = "pass" if not report["failures"] else "fail"
    with open(args.json, "w") as f:
        json.dump(report, f, indent=1, sort_keys=True)
    print(f"[{report['mode']}-n{report['n']}] verdict={report['verdict']} "
          f"failures={len(report['failures'])} -> {args.json}", flush=True)
    sys.exit(0 if report["verdict"] == "pass" else 1)


if __name__ == "__main__":
    main()
