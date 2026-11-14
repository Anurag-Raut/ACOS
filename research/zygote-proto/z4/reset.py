#!/usr/bin/env python3
"""ZPROD1: the persistent zygote with a **single-session reset**.

The Z7 loss (30 vs 39 tasks/min) is the prototype's reset, which restarted the
whole cohort and therefore re-paid the zygote's pre-fork core on every round.
This driver runs the persistent variant (`ACOS_ZYGOTE_PERSIST=1`):

  keeper launcher  (`acos-art launch --apk …`, ACOS_ZYGOTE_APP + plan dir, N=0)
    └── bwrap → ZygoteApp (persistent; serves `spawn`/`kill`/`exit` on <plan>/cmd)
  session launcher k (foreign guest, ACOS_ZYGOTE_SESSION=<plan>/k)
    ← child k connects to launcher k's peer

and resets **one** session:

  kill <tag>          the zygote kills that session's process group and reaps it
  wipe <plan>/<tag>/run + sock and the contract files   (fresh run root)
  restart launcher <tag>                               (new peer, new contract)
  spawn <tag>         the zygote forks one fresh child

Checks (all must hold, no partial credit):
  * after the reset, tag R reads count 0, its child pid changed, and
  * every sibling's child pid, launcher and counter are **unchanged**,
  * the zygote's pid and its boot epoch are unchanged (the reset did not
    restart the cohort), and no process of the old child survives.

    python3 research/zygote-proto/z4/reset.py --n 2 --reset 0 --out <dir>/<label> \
        --json <dir>/<label>.json
"""
import argparse
import importlib.util
import json
import os
import shutil
import sys
import time

HERE = os.path.dirname(os.path.abspath(__file__))
DRIVE = os.path.join(HERE, "drive.py")
spec = importlib.util.spec_from_file_location("z4drive", DRIVE)
drive = importlib.util.module_from_spec(spec)
spec.loader.exec_module(drive)

APK = drive.APK
ART = drive.ART
CACHE = drive.CACHE


def env_for(tag, out_dir, session_dir=None, plan=None, n=None, zygote_build=None,
            persist=False):
    env = drive.launch_env(
        "zygote", session_dir=session_dir, plan=plan, n=n,
        zygote_build=zygote_build, out_dir=out_dir, tag=tag)
    if persist:
        env["ACOS_ZYGOTE_PERSIST"] = "1"
    # `launch_env` sets ACOS_ZYGOTE_SESSION unconditionally; a None value would
    # reach Popen as a non-string. Unset it for the keeper (no session of its own).
    return {k: v for k, v in env.items() if v is not None}


def spawn_launcher(tag, out_dir, session_dir=None, plan=None, n=None,
                   zygote_build=None, persist=False):
    os.makedirs(out_dir, exist_ok=True)
    cmd = [ART, "launch", "--apk", APK, "--until", "activity-resumed",
           "--out", out_dir, "--timeout", "300"]
    env = env_for(tag, out_dir, session_dir, plan, n, zygote_build, persist)
    err = open(os.path.join(out_dir, f"launcher-{tag}.err"), "wb")
    return drive.subprocess.Popen(cmd, cwd=drive.REPO, env=env, stdin=drive.subprocess.PIPE,
                                  stdout=drive.subprocess.PIPE, stderr=err, bufsize=0)


def read_kv(path):
    out = {}
    try:
        with open(path) as f:
            for token in f.read().split():
                if "=" in token:
                    k, v = token.split("=", 1)
                    out[k] = v
    except OSError:
        return None
    return out


def read_pid(path):
    """`<plan>/<tag>/pid` holds a bare integer (Z4's contract), not key=value."""
    try:
        with open(path) as f:
            return int(f.read().strip())
    except (OSError, ValueError):
        return None


def count_text(tree):
    nodes = []
    drive.find_nodes(tree.get("tree") or {}, nodes,
                     lambda n: str(n.get("id", "")).endswith(":id/count"))
    return nodes[0].get("text") if nodes else None


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--n", type=int, default=2)
    ap.add_argument("--reset", type=int, default=0)
    ap.add_argument("--out", required=True)
    ap.add_argument("--json", required=True)
    ap.add_argument("--zygote", default=os.path.join(CACHE, "zprod1-build"))
    ap.add_argument("--timeout", type=float, default=180.0)
    args = ap.parse_args()

    root = os.path.abspath(args.out)
    os.makedirs(root, exist_ok=True)
    plan = os.path.join(root, "sessions")
    os.makedirs(plan, exist_ok=True)
    report = {"mode": "zygote-persist", "n": args.n, "reset_tag": args.reset,
              "knobs": {"ACOS_ZYGOTE_PERSIST": "1", "ACOS_ZYGOTE_APP": args.zygote},
              "failures": [], "sessions": {},
              "started_utc": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime())}

    def fail(msg):
        report["failures"].append(msg)
        print(f"FAIL {msg}", file=sys.stderr, flush=True)

    # ---- 1. plan dir, command FIFO, session dirs ---------------------------
    for k in range(args.n):
        for sub in ("run", "sock"):
            os.makedirs(os.path.join(plan, str(k), sub), exist_ok=True)
    cmd_path = os.path.join(plan, "cmd")
    if os.path.exists(cmd_path):
        os.remove(cmd_path)
    os.mkfifo(cmd_path, 0o600)
    # O_RDWR: neither side blocks on open, and the FIFO does not signal EOF when
    # one writer restarts (the keeper never sees a spurious `cmd eof`).
    cmd_fd = os.open(cmd_path, os.O_RDWR)
    cmd_file = os.fdopen(cmd_fd, "w", buffering=1)

    def send(line):
        cmd_file.write(line + "\n")

    procs, sessions = {}, {}
    keeper = None

    def start_session(tag):
        out_dir = os.path.join(root, f"s{tag}")
        procs[tag] = spawn_launcher(tag, out_dir,
                                    session_dir=os.path.join(plan, str(tag)))
        if not drive.wait_file(os.path.join(plan, str(tag), "argv"), args.timeout):
            raise SystemExit(f"session {tag}: no argv contract")
        drive.write_binds(plan, tag)
        send(f"spawn {tag}")
        spawned = os.path.join(plan, str(tag), "spawned")
        deadline = time.time() + args.timeout
        info = None
        while time.time() < deadline:
            info = read_kv(spawned)
            if info:
                break
            time.sleep(0.02)
        if not info:
            raise SystemExit(f"session {tag}: zygote never acked spawn")
        sessions[tag] = drive.Session(procs[tag], args.timeout)
        return info

    try:
        # ---- 2. keeper (the one process that owns the zygote) --------------
        t0 = time.perf_counter()
        keeper_out = os.path.join(root, "keeper")
        keeper = spawn_launcher("keeper", keeper_out, plan=plan, n=0,
                                zygote_build=args.zygote, persist=True)
        report["keeper_pid"] = keeper.pid
        if not drive.wait_file(os.path.join(plan, "ready"), args.timeout):
            report["logs"] = collect_logs(root)
            raise SystemExit("zygote never wrote <plan>/ready")
        report["zygote_ready_ms"] = round((time.perf_counter() - t0) * 1000.0, 1)
        report["zygote_pid"] = int(open(os.path.join(plan, "zygote.pid")).read().strip())
        ready = read_kv(os.path.join(plan, "ready")) or {}
        report["zygote_epoch"] = ready.get("epoch")
        report["zygote_boot_ms"] = ready.get("boot_ms")

        # ---- 3. sessions S(k): tree 0 -> tap -> 1 --------------------------
        for k in range(args.n):
            spawn = start_session(k)
            sess = sessions[k]
            rec = {"tag": k, "spawn": spawn}
            started = time.perf_counter()
            while True:
                raw = sess.line().strip()
                if raw.startswith("{") and '"ready"' in raw:
                    rec["ready"] = json.loads(raw)
                    break
            rec["ready_ms"] = round((time.perf_counter() - started) * 1000.0, 1)
            tree, _, _ = sess.call({"cmd": "tree"})
            rec["count_initial"] = count_text(tree)
            shot0, _, _ = sess.call({"cmd": "screenshot"})
            rec["md5_initial"] = shot0.get("md5")
            rec["frame_initial"] = shot0.get("frame_number")
            if rec["count_initial"] != "0":
                fail(f"session {k}: initial count {rec['count_initial']!r} != '0'")
            nodes = []
            drive.find_nodes(tree.get("tree") or {}, nodes,
                             lambda n: str(n.get("id", "")).endswith(":id/inc"))
            if not nodes:
                fail(f"session {k}: no :id/inc node")
                report["sessions"][k] = rec
                continue
            x, y = drive.center(nodes[0])
            rec["tap_ok"] = bool(sess.call({"cmd": "tap", "x": x, "y": y})[0].get("ok"))
            tree2, _, _ = sess.call({"cmd": "tree"})
            rec["count_after_tap"] = count_text(tree2)
            if rec["count_after_tap"] != "1":
                fail(f"session {k}: count after tap {rec['count_after_tap']!r} != '1'")
            shot, _, _ = sess.call({"cmd": "screenshot"})
            rec["md5_after_tap"] = shot.get("md5")
            rec["child_pid"] = read_pid(os.path.join(plan, str(k), "pid"))
            report["sessions"][k] = rec

        before = {k: dict(report["sessions"][k]) for k in report["sessions"]}
        zygote_pid_before = report["zygote_pid"]

        # ---- 4. the reset: ONE child dies, one fresh child is forked ------
        rtag = args.reset
        t_reset = time.perf_counter()
        reset = {"tag": rtag,
                 "old_child_pid": before.get(rtag, {}).get("child_pid"),
                 "siblings_before": {k: {"child_pid": before.get(k, {}).get("child_pid"),
                                         "count": before.get(k, {}).get("count_after_tap")}
                                     for k in before if k != rtag}}
        reaped = os.path.join(plan, str(rtag), "reaped")
        for p in (reaped, os.path.join(plan, str(rtag), "spawned"),
                  os.path.join(plan, str(rtag), "pid")):
            if os.path.exists(p):
                os.remove(p)
        send(f"kill {rtag}")
        deadline = time.time() + args.timeout
        reap = None
        while time.time() < deadline:
            reap = read_kv(reaped)
            if reap:
                break
            time.sleep(0.02)
        if not reap:
            fail("zygote never acked kill")
        reset["reap"] = reap
        reset["kill_ms"] = round((time.perf_counter() - t_reset) * 1000.0, 1)

        # The old launcher must go before its run root is wiped; the zygote and
        # every sibling launcher stay untouched.
        old_proc = procs[rtag]
        old_proc.kill()
        old_proc.wait(timeout=30)
        for sub in ("run", "sock"):
            shutil.rmtree(os.path.join(plan, str(rtag), sub), ignore_errors=True)
            os.makedirs(os.path.join(plan, str(rtag), sub), exist_ok=True)
        for f in ("argv", "binds", "session.json"):
            p = os.path.join(plan, str(rtag), f)
            if os.path.exists(p):
                os.remove(p)
        reset["run_root_wiped"] = True
        t_re = time.perf_counter()
        spawn = start_session(rtag)
        reset["spawn"] = spawn
        reset["respawn_ms"] = round((time.perf_counter() - t_re) * 1000.0, 1)
        sess = sessions[rtag]
        while True:
            raw = sess.line().strip()
            if raw.startswith("{") and '"ready"' in raw:
                reset["ready"] = json.loads(raw)
                break
        tree, _, _ = sess.call({"cmd": "tree"})
        reset["count_after_reset"] = count_text(tree)
        shot, _, _ = sess.call({"cmd": "screenshot"})
        reset["md5_after_reset"] = shot.get("md5")
        reset["new_child_pid"] = read_pid(os.path.join(plan, str(rtag), "pid"))
        reset["reset_total_ms"] = round((time.perf_counter() - t_reset) * 1000.0, 1)
        report["reset"] = reset

        # ---- 5. the assertions that make the increment real ---------------
        if reset["count_after_reset"] != "0":
            fail(f"reset session {rtag}: count {reset['count_after_reset']!r} != '0' "
                 f"(state leaked from the previous task)")
        if str(reset["new_child_pid"]) == str(reset["old_child_pid"]):
            fail("reset: the child pid did not change (the same process answered)")
        # A provably fresh state, the strongest form available: the new child's
        # first frame is byte-identical to a brand-new session's first frame
        # (`md5_initial`) and different from the post-tap frame of the old child.
        if reset["md5_after_reset"] != before[rtag].get("md5_initial"):
            fail("reset: the new child's first frame is not the initial frame "
                 f"({reset['md5_after_reset']} != {before[rtag].get('md5_initial')})")
        if reset["md5_after_reset"] == before[rtag].get("md5_after_tap"):
            fail("reset: the frame after the reset is the OLD child's post-tap frame")
        # A fresh child from the same zygote: same epoch, same zygote pid.
        ready_now = read_kv(os.path.join(plan, "ready")) or {}
        reset["zygote_epoch_after"] = ready_now.get("epoch")
        reset["zygote_pid_after"] = int(open(os.path.join(plan, "zygote.pid")).read().strip())
        if reset["zygote_pid_after"] != zygote_pid_before:
            fail("reset restarted the zygote (pid changed)")
        if reset["zygote_epoch_after"] != report["zygote_epoch"]:
            fail("reset restarted the zygote (boot epoch changed)")
        # Siblings: same launcher, same child pid, same counter.
        for k in before:
            if k == rtag:
                continue
            s = sessions[k]
            rec = report["sessions"][k]
            tree_k, _, _ = s.call({"cmd": "tree"})
            now_count = count_text(tree_k)
            now_pid = read_pid(os.path.join(plan, str(k), "pid"))
            rec["count_after_reset"] = now_count
            rec["child_pid_after_reset"] = now_pid
            if str(now_pid) != str(before[k].get("child_pid")):
                fail(f"sibling {k}: child pid changed {before[k].get('child_pid')} -> {now_pid}")
            if now_count != "1":
                fail(f"sibling {k}: counter {now_count!r} != '1' after the other session's reset")
        report["elapsed_s"] = round(time.perf_counter() - t0, 2)
    finally:
        try:
            cmd_file.write("exit\n")
            cmd_file.flush()
        except Exception:
            pass
        for tag, p in procs.items():
            try:
                p.kill()
                p.wait(timeout=10)
            except Exception:
                pass
        try:
            if keeper is not None:
                keeper.kill()
                keeper.wait(timeout=10)
        except Exception:
            pass
        try:
            cmd_file.close()
        except Exception:
            pass

    report["verdict"] = "PASS" if not report["failures"] else "FAIL"
    with open(args.json, "w") as f:
        json.dump(report, f, indent=1)
    print(json.dumps({k: report[k] for k in ("verdict", "failures", "sessions", "reset")},
                     indent=1)[:4000])
    return 0 if report["verdict"] == "PASS" else 1


def collect_logs(root):
    out = {}
    for base, _dirs, names in os.walk(root):
        for name in names:
            if not name.endswith((".err", ".out")):
                continue
            p = os.path.join(base, name)
            try:
                with open(p, errors="replace") as f:
                    out[os.path.relpath(p, root)] = f.read()[-4000:]
            except OSError:
                pass
    return out


if __name__ == "__main__":
    sys.exit(main())
