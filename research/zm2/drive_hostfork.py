#!/usr/bin/env python3
"""ZM2 prototype C driver: N sessions served by N launchers forked from ONE
warm host template (`ACOS_HOST_ZYGOTE_PLAN`).

Per session: tree must read 0, a tap must give 1, every other not-yet-tapped
session must still read 0, and both screenshots must be byte-identical to the
normal path's (`39d1e4e3…` initial / `9f3406c1…` after tap). Memory is sampled
while all N sessions are held, with the same grouping as part1.py.

    python3 research/zm2/drive_hostfork.py --n 5 --out DIR --json J
"""
import argparse
import json
import os
import subprocess
import sys
import time

HERE = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.abspath(os.path.join(HERE, "..", ".."))
CACHE = os.environ.get("ACOS_CACHE", "/home/Qui/Projects/acos/.cache")
APK = os.path.join(REPO, "fixtures/build/apps/counter.apk")
ART = os.environ.get("ACOS_ART_BIN", os.path.join(CACHE, "zm2c-target/release/acos-art"))

sys.path.insert(0, HERE)
import part1  # noqa: E402  (installs the detailed host sampler on z4's driver)
z4 = part1.z4

MD5_INITIAL = "39d1e4e3633d98d76b992edb4b250a78"
MD5_TAPPED = "9f3406c1627beef270f8cfddf9f61973"


class ProcView:
    """What `z4.Session` needs from a `subprocess.Popen` (per-session pipes)."""

    def __init__(self, stdin, stdout, template):
        self.stdin = stdin
        self.stdout = stdout
        self._template = template

    def wait(self, timeout=None):
        return self._template.wait(timeout=timeout)

    def kill(self):
        try:
            self._template.kill()
        except OSError:
            pass


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--n", type=int, required=True)
    ap.add_argument("--out", required=True)
    ap.add_argument("--json", required=True)
    ap.add_argument("--timeout", type=float, default=240.0)
    args = ap.parse_args()

    root = os.path.abspath(args.out)
    os.makedirs(root, exist_ok=True)
    plan_dir = os.path.join(root, "plan")
    os.makedirs(plan_dir, exist_ok=True)

    report = {"mode": "hostfork", "n": args.n, "sessions": {}, "failures": [],
              "started_utc": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime())}

    slots, pass_fds, keep, sessions = [], [], [], {}
    writers, readers = {}, {}
    t0 = time.perf_counter()
    for k in range(args.n):
        in_r, in_w = os.pipe()
        out_r, out_w = os.pipe()
        err_path = os.path.join(root, f"launcher-{k}.err")
        err_fd = os.open(err_path, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o644)
        sdir = os.path.join(root, f"s{k}")
        os.makedirs(sdir, exist_ok=True)
        slots.append({
            "tag": k,
            "apk": APK,
            "until": "activity-resumed",
            "timeout_s": 300,
            "out": sdir,
            "stdin_fd": in_r,
            "stdout_fd": out_w,
            "stderr_fd": err_fd,
            "env": [
                ["ACOSLAUNCH_GUEST_STDOUT_FILE", os.path.join(root, f"guest-{k}.out")],
                ["ACOSLAUNCH_GUEST_STDERR_FILE", os.path.join(root, f"guest-{k}.err")],
            ],
        })
        pass_fds += [in_r, out_w, err_fd]
        keep += [(in_r, in_w, out_r, out_w, err_fd)]

    plan = {"n": args.n, "sessions": slots}
    plan_path = os.path.join(plan_dir, "plan.json")
    with open(plan_path, "w") as f:
        json.dump(plan, f, indent=1)

    env = dict(os.environ)
    env.update({
        "ACOS_FONT_DIR": "/home/Qui/Projects/acos/.cache/boot/android-34/fonts",
        "ACOSLAUNCH_INTERACTIVE": "1",
        "ACOSLAUNCH_LIVE_PROBE": "1",
        "ACOSLAUNCH_FRAME_WAIT_SECS": "30",
        "ACOS_INPUT_ACK_MS": "2500",
        "ACOS_HOST_ZYGOTE_PLAN": plan_dir,
    })
    for extra in ("ACOS_ZYGOTE_APP", "ACOS_ZYGOTE_SESSION", "ACOS_ZYGOTE_PLAN_DIR",
                  "ACOS_ZYGOTE_N", "ACOSLAUNCH_HOLD_MS"):
        env.pop(extra, None)
    template_err = open(os.path.join(root, "template.err"), "wb")
    template = subprocess.Popen(
        [ART, "launch", "--apk", APK, "--until", "activity-resumed",
         "--out", os.path.join(plan_dir, "template-out"), "--timeout", "600"],
        cwd=REPO, env=env, stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL,
        stderr=template_err, pass_fds=tuple(pass_fds),
    )
    report["spawn_ms"] = round((time.perf_counter() - t0) * 1000.0, 1)

    for k, (in_r, in_w, out_r, out_w, err_fd) in enumerate(keep):
        os.close(in_r)
        os.close(out_w)
        os.close(err_fd)
        stdin = os.fdopen(in_w, "wb", buffering=0)
        stdout = os.fdopen(out_r, "rb", buffering=0)
        view = ProcView(stdin, stdout, template)
        sessions[k] = (z4.Session(view, args.timeout), stdin)

    def finish():
        for k, (sess, stdin) in sessions.items():
            try:
                reply, _, _ = sess.call({"cmd": "close"})
                report["sessions"].setdefault(k, {})["close"] = {"ok": bool(reply.get("ok"))}
            except BaseException as e:
                report["sessions"].setdefault(k, {})["close"] = {"error": str(e)}
            try:
                stdin.close()
            except OSError:
                pass
        try:
            code = template.wait(timeout=120)
        except subprocess.TimeoutExpired:
            template.kill()
            code = template.wait()
        report["template_exit"] = code
        report["verdict"] = "pass" if not report["failures"] else "fail"
        with open(args.json, "w") as f:
            json.dump(report, f, indent=1, sort_keys=True)
        print(f"[hostfork-n{args.n}] verdict={report['verdict']} "
              f"failures={len(report['failures'])} -> {args.json}", flush=True)
        sys.exit(0 if report["verdict"] == "pass" else 1)

    # Ready + initial tree/screenshot for every session (sequential reader).
    for k in range(args.n):
        sess, _ = sessions[k]
        z4.run_session(sess, k, report, args.timeout)
    if report["failures"]:
        report["memory"] = z4.sample_memory(os.getpid(), f"hostfork-n{args.n}")
        finish()
    report["ready_ms_max"] = max((s.get("ready_ms", 0)
                                  for s in report["sessions"].values()), default=0)

    # Ordered isolation matrix + tap, same as Z3/Z4.
    for k in range(args.n):
        sess, _ = sessions[k]
        z4.tap(sess, report["sessions"][k])
        got = z4.read_count(sess, report["sessions"][k], f"tree_after_tap_{k}")
        report["sessions"][k]["count_after_tap"] = got
        if got != "1":
            report["failures"].append(f"session {k}: count after tap {got!r}, expected '1'")
        for j in range(k + 1, args.n):
            other_sess, _ = sessions[j]
            other = z4.read_count(other_sess, report["sessions"][j],
                                  f"tree_isolated_after_tap_{k}")
            report["sessions"][j].setdefault("isolation_reads", {})[k] = other
            if other != "0":
                report["failures"].append(
                    f"session {j}: count {other!r} changed after session {k}'s tap")
        shot, _, _ = sess.call({"cmd": "screenshot"})
        report["sessions"][k]["steps"]["screenshot_after_tap"] = shot
        report["sessions"][k]["md5_after_tap"] = shot.get("md5")
        report["sessions"][k]["frame_after_tap"] = shot.get("frame_number")
        print(f"[{k}] tap ok={report['sessions'][k]['tap_ok']} count={got} "
              f"md5={shot.get('md5')}", flush=True)

    for k in range(args.n):
        for step, want in (("md5_initial", MD5_INITIAL), ("md5_after_tap", MD5_TAPPED)):
            got = report["sessions"][k].get(step)
            if got != want:
                report["failures"].append(
                    f"session {k}: {step} {got!r} != normal path {want!r}")
    all0 = {k: report["sessions"][k].get("md5_initial") for k in range(args.n)}
    all1 = {k: report["sessions"][k].get("md5_after_tap") for k in range(args.n)}
    report["md5_initial_all_equal"] = len(set(all0.values())) == 1
    report["md5_after_tap_all_equal"] = len(set(all1.values())) == 1

    time.sleep(0.5)
    report["memory"] = z4.sample_memory(os.getpid(), f"hostfork-n{args.n}")
    report["total_ms"] = round((time.perf_counter() - t0) * 1000.0, 1)
    finish()


if __name__ == "__main__":
    main()
