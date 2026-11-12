#!/usr/bin/env python3
"""p9.py -- P9: opt-in `settle:"fast"` for launch and reset.

M4 (warm launch -> first correct screen) and M6 (reset -> first correct
screen), fast vs strict, plus the two freshness proofs, all over the real
transport an agent uses: `acos serve` + newline-JSON on its unix socket, the
real verbs (`acos_launch`, `acos_tap`, `acos_observe_tree`, `acos_reset`,
`acos_destroy`) and the release binaries.

Definitions (P7/P4, unchanged):

  M4  daemon up, app not running -> acos_launch -> the reply's own tree is the
      verified count-0 screen (12 nodes, count text "0"). With `pixels:true`
      the reply's frame md5 must also be the reference md5, which is P7's
      stricter form of the same number.
  M6  count=1 on screen -> acos_reset -> the reply's own tree is the count-0
      screen. The "end to end" column adds the strict re-read P4 also reported.

A row is `ok` only when its tree is verified **and** the post-frame strict
`acos_observe_tree` still reads count 0 (P6's rule). Nothing is accepted on a
polite `ok:true`.

Usage (inside run-exclusive.sh + guarded.sh):

    p9.py ref      --out DIR
    p9.py m4       --out DIR --settle fast --reps 24 --tag fast
    p9.py m6       --out DIR --settle fast --reps 21 --tag fast
    p9.py freshness --out DIR --settle fast --reps 20
    p9.py analyze  --dir DIR
"""

import argparse
import json
import os
import socket
import subprocess
import sys
import time
from datetime import datetime, timezone
from pathlib import Path

HERE = Path(__file__).resolve().parent
REPO = HERE.parent.parent.parent.parent          # results/art-runtime/evidence/P9
sys.path.insert(0, str(REPO / "tools"))
import benchlib  # noqa: E402

# P9 keeps its build in a dedicated target dir (CARGO_TARGET_DIR); `P9_TARGET`
# names it so the harness never silently measures a stale binary.
TARGET = Path(os.environ.get("P9_TARGET", str(REPO / "target"))).resolve()
BIN = TARGET / "release/acos"
ART_BIN = TARGET / "release/acos-art"
APK = REPO / "fixtures/build/apps/counter.apk"
# Long-run scratch root: never /tmp, and short enough for the run-root sockets
# (`sun_path` is 108 bytes; a deep worktree path is what breaks cleanup.rs).
SCRATCH = Path("/home/Qui/Projects/acos/.cache/p9-run")
COUNT_RID = "dev.acos.fixtures.counter:id/count"
EXPECT_TEXT = "0"
EXPECT_NODES = 12


def utc():
    return datetime.now(timezone.utc).isoformat(timespec="milliseconds")


def host_now():
    return {**benchlib.read_meminfo(), **benchlib.read_loadavg()}


class Daemon:
    """The real transport: newline JSON on the daemon's unix socket."""

    def __init__(self, path, timeout=180.0):
        self.sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        self.sock.settimeout(timeout)
        self.sock.connect(str(path))
        self.file = self.sock.makefile("rwb")
        self.n = 0

    def call(self, tool, args):
        self.n += 1
        t0 = time.perf_counter()
        self.file.write((json.dumps({"id": self.n, "tool": tool, "args": args},
                                    separators=(",", ":")) + "\n").encode())
        self.file.flush()
        line = self.file.readline()
        ms = (time.perf_counter() - t0) * 1000.0
        if not line:
            raise ConnectionError("daemon closed the connection during %s" % tool)
        return ms, json.loads(line)

    def close(self):
        try:
            self.file.close()
        finally:
            self.sock.close()


def nodes_of(tree):
    def walk(ns):
        for n in ns or []:
            if isinstance(n, dict):
                yield n
                yield from walk(n.get("children"))
    return list(walk((tree or {}).get("nodes")))


def count_text(tree):
    for n in nodes_of(tree):
        rid = n.get("resource_id") or ""
        if rid == COUNT_RID or rid.endswith(":id/count"):
            return n.get("text")
    return None


def tree_ok(tree):
    return count_text(tree) == EXPECT_TEXT and len(nodes_of(tree)) == EXPECT_NODES


def result(reply):
    return (reply.get("envelope") or {}).get("result") or {}


def error_code(reply):
    return (reply.get("envelope") or {}).get("error", {}).get("code") or reply.get("error", {}).get("code")


def start_daemon(tag):
    SCRATCH.mkdir(parents=True, exist_ok=True)
    sock = SCRATCH / ("s-%s.sock" % tag)
    if sock.exists():
        sock.unlink()
    env = dict(os.environ)
    env["ACOS_RUNTIME_DIR"] = str(SCRATCH / ("rt-%s" % tag))
    env["ACOS_CACHE_DIR"] = str(SCRATCH / ("cache-%s" % tag))
    t0 = time.perf_counter()
    proc = subprocess.Popen(
        [str(BIN), "serve", "--socket", str(sock), "--idle-timeout", "600",
         "--max-sessions", "8"],
        env=env, stdout=subprocess.DEVNULL,
        stderr=open(SCRATCH / ("serve-%s.err" % tag), "wb"))
    while time.perf_counter() - t0 < 30.0:
        if sock.exists():
            return proc, sock, (time.perf_counter() - t0) * 1000.0
        if proc.poll() is not None:
            raise RuntimeError("serve exited during startup")
        time.sleep(0.0005)
    raise RuntimeError("serve socket never appeared")


def stop_daemon(proc):
    if proc is None:
        return
    proc.terminate()
    try:
        proc.wait(10)
    except subprocess.TimeoutExpired:
        proc.kill()
        proc.wait(10)


def launch_args(settle, pixels):
    args = {"settle": settle}
    if pixels:
        args["pixels"] = True
    return args


def launch_row(d, base, settle, pixels, ref_md5, tag):
    """One M4 rep: launch -> the reply's tree is the verified count-0 screen."""
    row = {"rep_tag": tag, "settle_requested": settle, "pixels": bool(pixels),
           "host": host_now(), "utc": utc()}
    args = {"base": base, "apk": str(APK)}
    args.update(launch_args(settle, pixels))
    t0 = time.perf_counter()
    ms, reply = d.call("acos_launch", args)
    row["launch_wall_ms"] = ms
    row["launch_total_from_send_ms"] = (time.perf_counter() - t0) * 1000.0
    res = result(reply)
    if not (reply.get("envelope") or {}).get("ok"):
        row["error"] = error_code(reply) or "launch-failed"
        return row
    row["launch_tree_ok"] = tree_ok(res.get("tree"))
    row["launch_count_text"] = count_text(res.get("tree"))
    row["launch_nodes"] = len(nodes_of(res.get("tree")))
    row["settle"] = res.get("settle")
    frame = res.get("frame") or {}
    row["frame"] = {k: frame.get(k) for k in
                    ("frame_seq", "md5", "buffer_generation", "frame_number",
                     "pixels", "identity")}
    row["frame_md5_matches_reference"] = (frame.get("md5") == ref_md5) if pixels else None
    row["timing"] = (res.get("timing") or {}).get("total_ms")
    # A later strict read proves the tree the reply returned is the settled
    # screen (P6's rule). It is outside the M4 clock.
    tms, verify = d.call("acos_observe_tree", {"base": base, "settle": "strict"})
    vres = result(verify)
    row["verify_wall_ms"] = tms
    row["verify_count_text"] = count_text(vres.get("tree"))
    row["verify_tree_ok"] = tree_ok(vres.get("tree"))
    row["ok"] = bool(row["launch_tree_ok"] and row["verify_tree_ok"]
                     and (row["frame_md5_matches_reference"] is not False))
    return row


def make_reference(d, scratch_dir):
    """P7's reference rule: count 0, two identical settled captures, and the
    launch reply's own frame must be that same screen."""
    base = {"session_id": "s_ref"}
    ms, reply = d.call("acos_launch", {"base": base, "apk": str(APK),
                                       "pixels": True, "settle": "strict"})
    res = result(reply)
    if not tree_ok(res.get("tree")):
        raise RuntimeError("reference launch tree is not the count-0 screen: %r"
                           % count_text(res.get("tree")))
    shots = []
    for i in (1, 2):
        p = scratch_dir / ("ref-%d.png" % i)
        _, r = d.call("acos_observe", {"base": base, "tree": False, "png": True,
                                       "screenshot": str(p)})
        shots.append((result(r).get("frame") or {}, p))
    md5s = [s[0].get("md5") for s in shots]
    if md5s[0] != md5s[1]:
        raise RuntimeError("two consecutive settled captures differ: %s" % md5s)
    launch_md5 = (res.get("frame") or {}).get("md5")
    if launch_md5 != md5s[0]:
        raise RuntimeError("launch frame %s != settled capture %s"
                           % (launch_md5, md5s[0]))
    return {"md5": md5s[0], "nodes": len(nodes_of(res["tree"])),
            "count_text": count_text(res["tree"]), "launch_wall_ms": ms,
            "settle": res.get("settle")}


def tap_to(d, base, n):
    """Drive the counter to `n` with fast taps; returns the observed text."""
    seen = count_text(result(d.call("acos_observe_tree", {"base": base,
                                                          "settle": "fast"})[1]).get("tree"))
    for _ in range(n):
        _, r = d.call("acos_tap", {"base": base, "id": "inc", "settle": "fast",
                                   "observe": {"tree": True, "png": False}})
        seen = count_text(result(r).get("tree"))
    return seen


# ------------------------------------------------------------------ phases
def phase_ref(out):
    proc, sock, _ = start_daemon("ref")
    try:
        d = Daemon(sock)
        base = {"session_id": "s_ref"}
        ref = make_reference(d, SCRATCH)
        d.call("acos_destroy", {"base": base})
        d.close()
    finally:
        stop_daemon(proc)
    ref["utc"] = utc()
    benchlib.write_json(str(out / "ref.json"), ref)
    print("p9: reference md5 %s (%d nodes, count %r)"
          % (ref["md5"], ref["nodes"], ref["count_text"]), flush=True)
    return ref


def phase_m4(out, settle, pixels, reps, tag, ref):
    proc, sock, sock_ms = start_daemon("m4-%s" % tag)
    rows = []
    try:
        d = Daemon(sock)
        warm = {"session_id": "s_warm"}
        d.call("acos_launch", {"base": warm, "apk": str(APK), "settle": settle,
                               "pixels": pixels})
        d.call("acos_destroy", {"base": warm})
        for i in range(reps):
            base = {"session_id": "s_m4_%s_%d" % (tag, i)}
            row = launch_row(d, base, settle, pixels, ref["md5"], i)
            dms, dr = d.call("acos_destroy", {"base": base})
            row["destroy_wall_ms"] = dms
            row["destroy_clean"] = bool(result(dr).get("teardown", {}).get("verified"))
            rows.append(row)
            print("M4 %-10s rep %2d launch=%.1f settle=%s verified=%s"
                  % (tag, i, row.get("launch_wall_ms", -1),
                     (row.get("settle") or {}).get("bound"), row.get("ok")), flush=True)
        d.close()
    finally:
        stop_daemon(proc)
    payload = {"phase": "M4", "mode": tag, "settle_requested": settle,
               "pixels": bool(pixels), "rows": rows, "daemon_start_ms": sock_ms,
               "definition": "warm daemon, app not running -> acos_launch -> the reply's "
                             "own tree is the verified count-0 screen",
               "utc": utc()}
    benchlib.write_json(str(out / ("m4-%s.json" % tag)), payload)
    return payload


def phase_m6(out, settle, pixels, reps, tag):
    proc, sock, sock_ms = start_daemon("m6-%s" % tag)
    rows = []
    try:
        d = Daemon(sock)
        base = {"session_id": "s_m6_%s" % tag}
        d.call("acos_launch", {"base": base, "apk": str(APK), "settle": "strict"})
        for i in range(reps + 1):          # rep 0 is the discarded warm-up
            tap_to(d, base, 1)
            # Strict pre-reset read: "reset after count=1" must not rest on a
            # possibly mid-animation fast tap tree.
            before = count_text(result(d.call("acos_observe_tree",
                                              {"base": base, "settle": "strict"})[1])
                                .get("tree"))
            row = {"rep": i, "host": host_now(), "utc": utc(),
                   "settle_requested": settle, "pixels": bool(pixels),
                   "pre_reset_count_text": before}
            if i == 0:
                row["warmup"] = True
            t0 = time.perf_counter()
            args = {"base": base}
            args.update(launch_args(settle, pixels))
            ms, reply = d.call("acos_reset", args)
            row["reset_wall_ms"] = ms
            res = result(reply)
            if not (reply.get("envelope") or {}).get("ok"):
                row["error"] = error_code(reply) or "reset-failed"
                rows.append(row)
                continue
            row["reset_tree_ok"] = tree_ok(res.get("tree"))
            row["reset_count_text"] = count_text(res.get("tree"))
            row["reset_nodes"] = len(nodes_of(res.get("tree")))
            row["settle"] = res.get("settle")
            row["frame_seq"] = res.get("frame_seq")
            row["isolation"] = res.get("isolation")
            row["data_cleared"] = res.get("data_cleared")
            # P4's end-to-end form: a strict re-read after the reset.
            tms, verify = d.call("acos_observe_tree", {"base": base,
                                                       "settle": "strict"})
            row["verify_wall_ms"] = tms
            row["verify_tree_ok"] = tree_ok(result(verify).get("tree"))
            row["e2e_ms"] = row["reset_wall_ms"] + tms + (time.perf_counter() - t0) * 0.0
            row["ok"] = bool(row["reset_tree_ok"] and row["verify_tree_ok"])
            if not row.get("warmup"):
                rows.append(row)
            print("M6 %-10s rep %2d reset=%.1f settle=%s verified=%s"
                  % (tag, i, row["reset_wall_ms"],
                     (row.get("settle") or {}).get("bound"), row.get("ok")), flush=True)
        d.call("acos_destroy", {"base": base})
        d.close()
    finally:
        stop_daemon(proc)
    payload = {"phase": "M6", "mode": tag, "settle_requested": settle,
               "pixels": bool(pixels), "rows": rows, "daemon_start_ms": sock_ms,
               "definition": "count=1 on screen -> acos_reset -> the reply's own tree is "
                             "the count-0 screen; e2e adds a strict re-read",
               "utc": utc()}
    benchlib.write_json(str(out / ("m6-%s.json" % tag)), payload)
    return payload


def phase_freshness(out, settle, reps):
    """Two freshness proofs (P9's requirement):

    A. a **fast launch** after a previous session that read count=3 must return
       the new app's tree (count 0), never the previous session's tree;
    B. a **fast reset** after count=3 must return count 0.
    """
    proc, sock, _ = start_daemon("fresh")
    rows = {"launch_after_count3": [], "reset_after_count3": []}
    try:
        d = Daemon(sock)
        for i in range(reps):
            # A: drive a session to count=3, destroy it, then fast-launch.
            base = {"session_id": "s_fresh_%d" % i}
            d.call("acos_launch", {"base": base, "apk": str(APK), "settle": "fast"})
            seen = tap_to(d, base, 3)
            d.call("acos_destroy", {"base": base})
            row = {"rep": i, "previous_session_count_text": seen}
            _, reply = d.call("acos_launch", {"base": base, "apk": str(APK),
                                             "settle": settle})
            res = result(reply)
            row["launch_count_text"] = count_text(res.get("tree"))
            row["launch_tree_ok"] = tree_ok(res.get("tree"))
            row["settle"] = res.get("settle")
            row["ok"] = bool(seen == "3" and row["launch_tree_ok"])
            rows["launch_after_count3"].append(row)
            print("freshness A rep %2d prev=%s fast-launch=%s ok=%s"
                  % (i, seen, row["launch_count_text"], row["ok"]), flush=True)

            # B: same session, drive to count=3, then a fast reset. The
            # pre-reset state is read **strict**: a fast tap's own tree can be
            # mid-animation, and this proof must not depend on that.
            tap_to(d, base, 3)
            seen = count_text(result(d.call("acos_observe_tree",
                                            {"base": base, "settle": "strict"})[1])
                              .get("tree"))
            _, reply = d.call("acos_reset", {"base": base, "settle": settle})
            res = result(reply)
            row = {"rep": i, "pre_reset_count_text": seen,
                   "reset_count_text": count_text(res.get("tree")),
                   "reset_tree_ok": tree_ok(res.get("tree")),
                   "settle": res.get("settle"),
                   "frame_seq": res.get("frame_seq")}
            row["ok"] = bool(seen == "3" and row["reset_tree_ok"])
            rows["reset_after_count3"].append(row)
            print("freshness B rep %2d prev=%s fast-reset=%s ok=%s"
                  % (i, seen, row["reset_count_text"], row["ok"]), flush=True)
            d.call("acos_destroy", {"base": base})
        d.close()
    finally:
        stop_daemon(proc)
    payload = {"phase": "freshness", "settle": settle, "reps": reps, "rows": rows,
               "utc": utc(),
               "definition": "A: previous session at count=3 destroyed -> fast launch must "
                             "read count 0. B: count=3 -> fast reset must read count 0."}
    benchlib.write_json(str(out / ("freshness-%s.json" % settle)), payload)
    return payload


# ----------------------------------------------------------------- analysis
def pct(vals, q):
    return benchlib.percentile(sorted(vals), q)


def summarize(rows, key):
    vals = [r[key] for r in rows if isinstance(r.get(key), (int, float))]
    if not vals:
        return None
    med = pct(vals, 50)
    return {"n": len(vals), "p50": med, "p95": pct(vals, 95),
            "min": min(vals), "max": max(vals),
            "spread_pct": (max(vals) - min(vals)) / med * 100.0 if med else None}


def analyze(out):
    out = Path(out)
    res = {"utc": utc(), "tables": {}}
    rp = out / "ref.json"
    if rp.exists():
        res["reference"] = json.loads(rp.read_text())
    for phase, prefix in (("M4", "m4"), ("M6", "m6")):
        for p in sorted(out.glob("%s-*.json" % prefix)):
            payload = json.loads(p.read_text())
            rows = payload["rows"]
            key = "%s/%s" % (phase, payload["mode"])
            table = {
                "settle_requested": payload["settle_requested"],
                "pixels": payload["pixels"],
                "n_reps": len(rows),
                "n_ok": sum(1 for r in rows if r.get("ok")),
                "n_tree_ok": sum(1 for r in rows if r.get("launch_tree_ok") or r.get("reset_tree_ok")),
                "n_verify_ok": sum(1 for r in rows if r.get("verify_tree_ok")),
                "settle_bound": {},
                "errors": [r.get("error") for r in rows if r.get("error")],
            }
            for r in rows:
                bound = (r.get("settle") or {}).get("bound")
                table["settle_bound"][bound] = table["settle_bound"].get(bound, 0) + 1
            if phase == "M4":
                table["launch_wall_ms"] = summarize(rows, "launch_wall_ms")
                table["verify_wall_ms"] = summarize(rows, "verify_wall_ms")
                table["md5_matches"] = sum(1 for r in rows
                                           if r.get("frame_md5_matches_reference") is True)
            else:
                table["reset_wall_ms"] = summarize(rows, "reset_wall_ms")
                table["e2e_ms"] = summarize(rows, "e2e_ms")
                table["verify_wall_ms"] = summarize(rows, "verify_wall_ms")
                table["n_data_cleared"] = sum(1 for r in rows if r.get("data_cleared"))
            res["tables"][key] = table
    for p in sorted(out.glob("freshness-*.json")):
        payload = json.loads(p.read_text())
        for name, rows in payload["rows"].items():
            after = [r.get("launch_count_text") if "launch_count_text" in r
                     else r.get("reset_count_text") for r in rows]
            before = [r.get("previous_session_count_text") if "previous_session_count_text" in r
                      else r.get("pre_reset_count_text") for r in rows]
            counts = {}
            for text in after:
                counts[str(text)] = counts.get(str(text), 0) + 1
            res["tables"]["freshness/%s/%s" % (payload["settle"], name)] = {
                "n_reps": len(rows),
                "n_ok": sum(1 for r in rows if r.get("ok")),
                "count_texts": counts,
                "all_read_zero": all(t == "0" for t in after),
                "previous_saw_three": all(t == "3" for t in before),
            }
    benchlib.write_json(str(out / "summary.json"), res)
    print(json.dumps(res["tables"], indent=2)[:6000])
    return 0


def main(argv):
    ap = argparse.ArgumentParser()
    ap.add_argument("mode", choices=["ref", "m4", "m6", "freshness", "all", "analyze"])
    ap.add_argument("--out", default=str(HERE / "run"))
    ap.add_argument("--dir", default=None)
    ap.add_argument("--settle", default="fast", choices=["fast", "strict"])
    ap.add_argument("--pixels", action="store_true")
    ap.add_argument("--tag", default=None)
    ap.add_argument("--reps", type=int, default=24)
    a = ap.parse_args(argv)
    out = Path(a.out).resolve()
    out.mkdir(parents=True, exist_ok=True)
    SCRATCH.mkdir(parents=True, exist_ok=True)
    for p in (BIN, ART_BIN, APK):
        if not p.exists():
            print("p9: missing %s" % p, file=sys.stderr)
            return 2
    if "release" not in str(BIN):
        print("p9: refusing a non-release binary", file=sys.stderr)
        return 2
    if a.mode == "analyze":
        return analyze(Path(a.dir or out))
    tag = a.tag or ("%s%s" % (a.settle, "-px" if a.pixels else ""))
    meta = {"utc": utc(), "bin": str(BIN), "acos_sha256": benchlib.sha256_file(BIN),
            "acos_art_sha256": benchlib.sha256_file(ART_BIN),
            "apk_sha256": benchlib.sha256_file(APK),
            "host": host_now(), "git": subprocess.run(
                ["git", "-C", str(REPO), "rev-parse", "HEAD"],
                capture_output=True, text=True).stdout.strip()}
    benchlib.write_json(str(out / ("meta-%s.json" % tag)), meta)
    ref = None
    rp = out / "ref.json"
    if rp.exists():
        ref = json.loads(rp.read_text())
    if a.mode in ("ref", "all"):
        ref = phase_ref(out)
    if ref is None:
        print("p9: no ref.json in %s (run `p9.py ref` first)" % out, file=sys.stderr)
        return 2
    if a.mode in ("m4", "all"):
        phase_m4(out, a.settle, a.pixels, a.reps, tag, ref)
    if a.mode in ("m6", "all"):
        phase_m6(out, a.settle, a.pixels, a.reps, tag)
    if a.mode in ("freshness", "all"):
        phase_freshness(out, a.settle, a.reps)
    if a.mode == "all":
        # The two strict baselines, so one run produces the whole table.
        phase_m4(out, "strict", True, a.reps, "strict-px", ref)
        phase_m4(out, "strict", False, a.reps, "strict-nopx", ref)
        phase_m6(out, "strict", True, a.reps, "strict-px")
        phase_m6(out, "strict", False, a.reps, "strict-nopx")
        phase_freshness(out, "strict", a.reps)
    return analyze(out)


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
