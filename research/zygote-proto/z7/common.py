#!/usr/bin/env python3
"""Z7 shared cohort plumbing for the lifetime / loop / teardown measurements.

One place owns the four arms so every harness launches exactly the same way:

  a   exec path, no knobs (production default)
  ap  exec path + the Z7 fair heap seam (real ART argv heap flags and/or the
      guest post-first-frame explicit GC/trim); the knobs are named in the JSON
  c   zygote (Z4 seam) + ACOS_ZYGOTE_NO_STUB=1
  cp  zygote + the same guest post-first-frame explicit GC/trim

Nothing here changes the product: every arm-specific behaviour is an
environment variable read by `crates/acos-art/src/launch.rs` (OFF by default).
"""
import json
import os
import sys
import time

HERE = os.path.dirname(os.path.abspath(__file__))
PROTO = os.path.dirname(HERE)
Z4_DIR = os.path.join(PROTO, "z4")
Z6_DIR = os.path.join(PROTO, "z6")
for p in (Z4_DIR, Z6_DIR):
    if p not in sys.path:
        sys.path.insert(0, p)

import drive as z4  # noqa: E402

CACHE = os.environ.get("ACOS_CACHE", "/home/Qui/Projects/acos/.cache")
REPO = os.path.abspath(os.path.join(HERE, "..", "..", ".."))
ART = os.environ.get("ACOS_ART_BIN", os.path.join(CACHE, "z7-target/release/acos-art"))
ZYGOTE_BUILD = os.environ.get("ACOS_Z7_ZYGOTE_BUILD", os.path.join(CACHE, "z7-build"))
OUT_ROOT = os.environ.get("ACOS_Z7_RUNS", os.path.join(CACHE, "z7-runs"))
COUNTER = os.path.join(REPO, "fixtures/build/apps/counter.apk")
FORM = os.path.join(REPO, "fixtures/build/apps/form-validation.apk")

ARMS = ("a", "ap", "c", "cp")
MODES = {"a": "normal", "ap": "normal", "c": "zygote", "cp": "zygote"}


def app_profile(name):
    if name == "form-validation":
        return {
            "apk": FORM,
            "kind": "type",
            "tap_suffix": ":id/name",
            "assert_suffix": ":id/name",
            "assert_text": "Ada",
            "assert_mode": "contains",
            "type_text": "Ada",
        }
    return {
        "apk": COUNTER,
        "kind": "tap",
        "tap_suffix": ":id/inc",
        "assert_suffix": ":id/count",
        "assert_text": "1",
        "assert_mode": "exact",
        "type_text": None,
    }


def arm_env(arm):
    """The complete arm-specific environment overlay (recorded in every JSON)."""
    env = {}
    if arm in ("c", "cp"):
        env["ACOS_ZYGOTE_APP"] = ZYGOTE_BUILD
        env["ACOS_ZYGOTE_NO_STUB"] = "1"
    if arm in ("ap", "cp"):
        # The same guest-side release hook for both optimised arms: right after
        # the first frame ask ART for a background CC collection (the only
        # explicit-GC entry point that collects on this image; measured in Z7).
        env["ACOS_ART_GC_AFTER_FIRST_FRAME"] = "1"
        if os.environ.get("ACOS_Z7_GC_DELAY_MS"):
            env["ACOS_ART_GC_DELAY_MS"] = os.environ["ACOS_Z7_GC_DELAY_MS"]
        # (a') additionally gets the real ART argv heap flags only when asked:
        # the default (a') definition is the explicit post-first-frame release,
        # which is the closest fair equivalent of what the zygote's post-fork
        # path gets. Z7.md states which definition was measured.
        for knobs in (("ACOS_ART_XMS", "ACOS_Z7_AP_XMS"),
                      ("ACOS_ART_XMX", "ACOS_Z7_AP_XMX"),
                      ("ACOS_ART_HEAP_TARGET_UTILIZATION", "ACOS_Z7_AP_UTIL")):
            if os.environ.get(knobs[1]):
                env[knobs[0]] = os.environ[knobs[1]]
    return env


def spawn_cohort(arm, n, out, plan, app="counter"):
    """Start one cohort exactly as the Z4 driver does; return (procs, sessions, t0)."""
    profile = app_profile(app)
    z4.APK = profile["apk"]
    z4.ART = ART
    # Keep the driver's own module-level defaults in step with our paths.
    root = os.path.abspath(out)
    os.makedirs(root, exist_ok=True)
    os.makedirs(plan, exist_ok=True)
    if MODES[arm] == "zygote":
        for k in range(n):
            os.makedirs(os.path.join(plan, str(k), "run"), exist_ok=True)
            os.makedirs(os.path.join(plan, str(k), "sock"), exist_ok=True)
    overlay = arm_env(arm)
    for k, v in overlay.items():
        os.environ[k] = v
    procs, sessions = {}, {}
    t0 = time.perf_counter()

    def start(tag, zygote=False):
        out_dir = os.path.join(root, f"s{tag}")
        procs[tag] = z4.spawn_launcher(
            MODES[arm], tag, out_dir,
            session_dir=os.path.join(plan, str(tag)) if MODES[arm] == "zygote" else None,
            plan=plan if zygote else None, n=n,
            zygote_build=ZYGOTE_BUILD if zygote else None,
        )
        sessions[tag] = z4.Session(procs[tag], 300.0)

    if MODES[arm] == "normal":
        for k in range(n):
            start(k)
    else:
        for k in range(1, n):
            start(k)
            z4.wait_file(os.path.join(plan, str(k), "argv"))
            z4.write_binds(plan, k)
        start(0, zygote=True)
        z4.wait_file(os.path.join(plan, "0", "argv"))
        z4.write_binds(plan, 0)
    return procs, sessions, t0, overlay


def read_ready(sess, t0, timeout=300.0):
    """Read lines until the first ready reply (nothing else is requested first)."""
    deadline = time.time() + timeout
    while time.time() < deadline:
        raw = sess.line().strip()
        if not raw:
            continue
        if raw.startswith("{"):
            try:
                reply = json.loads(raw)
            except json.JSONDecodeError:
                continue
            if reply.get("cmd") == "ready":
                return reply, round((time.perf_counter() - t0) * 1000.0, 1)
    raise SystemExit("timed out waiting for ready")


def read_all_ready(sessions, t0):
    """Sequential ready reads; no tree/tap/screenshot before this returns, so every
    child's recorded first-frame time is the guest's own (not the driver's)."""
    ready = {}
    for k in sorted(sessions):
        reply, t_ms = read_ready(sessions[k], t0)
        ready[k] = {"reply": reply, "t_ready_ms": t_ms}
    return ready


def find_nodes(node, out, pred):
    if isinstance(node.get("nodes"), list):
        for n in node["nodes"]:
            find_nodes(n, out, pred)
        return
    if pred(node):
        out.append(node)
    for c in node.get("children") or []:
        find_nodes(c, out, pred)


def by_suffix(tree, suffix):
    found = []
    find_nodes(tree or {}, found, lambda n: str(n.get("id", "")).endswith(suffix))
    return found[0] if found else None


def node_text(node):
    if not node:
        return ""
    return str(node.get("text") or node.get("hint") or "")


def center(node):
    import re
    m = re.match(r"\[(-?\d+),(-?\d+)\]\[(-?\d+),(-?\d+)\]", node["bounds"])
    x1, y1, x2, y2 = (int(g) for g in m.groups())
    return (x1 + x2) // 2, (y1 + y2) // 2


def drop_png(reply):
    """Drop a capture PNG from the page cache and unlink it.

    Every tree/screenshot reply writes a ~9.5 MiB PNG. A loop of hundreds of
    tasks otherwise accumulates gigabytes of page cache, which `memory.current`
    charges to the job cgroup even though it is reclaimable file cache. The
    launcher's C1 buffer and the reply's md5 stay valid; only the file goes.
    """
    path = (reply or {}).get("png")
    if not path:
        return
    try:
        fd = os.open(path, os.O_RDONLY)
        try:
            os.posix_fadvise(fd, 0, 0, os.POSIX_FADV_DONTNEED)
        finally:
            os.close(fd)
    except OSError:
        pass
    try:
        os.unlink(path)
    except OSError:
        pass


def verify_task(sess, profile, tag, rec):
    """One real task: inspect -> input -> assert, with fresh frames and pixels.
    Returns (ok, detail). Never acknowledges as a pass."""
    t0 = time.perf_counter()
    tree, _, _ = sess.call({"cmd": "tree"})
    drop_png(tree)
    rec.setdefault("steps", {})["tree_initial"] = tree
    node = by_suffix(tree.get("tree"), profile["assert_suffix"])
    initial = node_text(node)
    if profile["kind"] == "tap":
        ok_initial = initial.strip() == "0"
    else:
        # The form's name field starts empty (hint is the placeholder).
        ok_initial = initial.strip() in ("", "Name")
    tapnode = by_suffix(tree.get("tree"), profile["tap_suffix"])
    if tapnode is None:
        rec["error"] = "missing tap node"
        return False, rec
    x, y = center(tapnode)
    tap, _, tap_ms = sess.call({"cmd": "tap", "x": x, "y": y})
    drop_png(tap)
    rec["tap_ms"] = round(tap_ms, 2)
    rec["tap_ok"] = bool(tap.get("ok"))
    if profile["kind"] == "type" and tap.get("ok"):
        typed, _, type_ms = sess.call({"cmd": "type_text", "text": profile["type_text"]})
        rec["type_ms"] = round(type_ms, 2)
        rec["type_ok"] = bool(typed.get("ok"))
    tree2, _, _ = sess.call({"cmd": "tree"})
    drop_png(tree2)
    rec.setdefault("steps", {})["tree_after"] = tree2
    node2 = by_suffix(tree2.get("tree"), profile["assert_suffix"])
    after = node_text(node2)
    if profile["assert_mode"] == "exact":
        ok_after = after.strip() == profile["assert_text"]
    else:
        ok_after = profile["assert_text"] in after
    shot, _, _ = sess.call({"cmd": "screenshot"})
    drop_png(shot)
    rec["screenshot_md5"] = shot.get("md5")
    rec["screenshot_ok"] = bool(shot.get("ok")) and bool(shot.get("md5"))
    rec["initial"] = initial
    rec["after"] = after
    rec["verified"] = bool(ok_initial and rec["tap_ok"] and ok_after
                          and rec["screenshot_ok"])
    rec["task_ms"] = round((time.perf_counter() - t0) * 1000.0, 2)
    rec["tag"] = tag
    return rec["verified"], rec


def close_cohort(sessions, procs, order=None, bound=20.0):
    """Close in the given order, then reap with ONE shared deadline.

    The close replies are requested in order (observable isolation), but the
    launcher exits are waited for in parallel with a single `bound`, because a
    (c) launcher that hangs after `ok` must not multiply the bound by N.
    """
    order = list(sessions) if order is None else list(order)
    out = {k: {} for k in sessions}
    for k in order:
        sess = sessions[k]
        sess.timeout = bound
        entry = out[k]
        t0 = time.perf_counter()
        try:
            reply, _, _ = sess.call({"cmd": "close"})
            entry["reply_ok"] = bool(reply.get("ok"))
        except BaseException as e:  # a hung/closed launcher is a recorded fact
            entry["reply_error"] = repr(e)
        entry["close_ms"] = round((time.perf_counter() - t0) * 1000.0, 1)
        try:
            procs[k].stdin.close()
        except Exception:
            pass
    deadline = time.perf_counter() + bound
    for k in order:
        entry = out[k]
        left = max(0.05, deadline - time.perf_counter())
        t0 = time.perf_counter()
        try:
            entry["exit"] = procs[k].wait(timeout=left)
        except Exception:
            procs[k].kill()
            entry["exit"] = procs[k].wait()
            entry["killed"] = True
        entry["exit_wait_ms"] = round((time.perf_counter() - t0) * 1000.0, 1)
    return out


def kill_tree(pid, sig=9):
    """Kill a process and every descendant (after a close that did not exit)."""
    import signal
    if pid is None:
        return []
    victims = z4.subtree(pid) if os.path.exists(f"/proc/{pid}") else [pid]
    killed = []
    for p in victims:
        try:
            os.kill(p, sig)
            killed.append(p)
        except OSError:
            pass
    return killed


def proc_stats(pid):
    """fd count, thread count, VmRSS for one pid (for the per-cycle leak check)."""
    try:
        fds = len(os.listdir(f"/proc/{pid}/fd"))
    except OSError:
        fds = None
    try:
        threads = len(os.listdir(f"/proc/{pid}/task"))
    except OSError:
        threads = None
    rss = None
    try:
        with open(f"/proc/{pid}/status") as f:
            for line in f:
                if line.startswith("VmRSS:"):
                    rss = int(line.split()[1])
                    break
    except OSError:
        pass
    return {"fds": fds, "threads": threads, "rss_kib": rss}


def find_zygote_host_pid(launcher0_pid):
    """The HOST pid of the zygote parent.

    `sessions/zygote.pid` is written by the zygote *inside* its PID namespace,
    so it is not a host pid (the file typically holds `1`). The host process is
    launcher 0's descendant whose cmdline runs `dev.acos.art.ZygoteApp` and whose
    /proc/comm is `main` (the dalvik process, before its children rename).
    """
    if launcher0_pid is None:
        return None
    for pid in z4.subtree(launcher0_pid):
        if pid == launcher0_pid:
            continue
        try:
            cmd = open(f"/proc/{pid}/cmdline", "rb").read().decode("utf-8", "replace")
            comm = open(f"/proc/{pid}/comm").read().strip()
        except OSError:
            continue
        if "ZygoteApp" in cmd and comm == "main":
            return pid
    return None


def zygote_pid(plan):
    try:
        return int(open(os.path.join(plan, "zygote.pid")).read().strip())
    except (OSError, ValueError):
        return None


def write_json(path, report):
    os.makedirs(os.path.dirname(path), exist_ok=True)
    with open(path, "w") as f:
        json.dump(report, f, indent=1, sort_keys=True)
