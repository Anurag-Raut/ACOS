#!/usr/bin/env python3
"""P11: per-case strict-settle latency and correctness, quiet vs event.

One held `acos serve` ART session per case. The strict switch is the serve
process's environment (`ACOS_STRICT_SETTLE=quiet|event`), so run this script
twice with the same binary. For each measured action we record:

  wall_ms        client publish -> strict reply
  settle         the reply's settle block / error
  tree_ok        an immediate `settle:fast` tree still shows the case's known
                 final state (the tap reply itself is settled at that moment)
  png_ok         an immediate frame-only capture is byte-identical to the
                 long-wait reference PNG for the same final state
  false_pass     settled reply + tree/png does not show the expected state

`delay1000` is the documented long-delay case: both rules return before 1000 ms,
so it is reported separately and verified with `wait_for`-style polling. The
infinite ProgressBar case is expected to time out (never settled).
"""
import argparse
import hashlib
import json
import os
import shutil
import sys
import time
from pathlib import Path

HERE = Path(__file__).resolve().parent
REPO = HERE.parents[3]
sys.path.insert(0, str(REPO / "tools"))
sys.path.insert(0, str(REPO / "tools/art-runtime"))
import benchlib          # noqa: E402
import d2_warm           # noqa: E402

APK = REPO / "fixtures/build/apps/settle-lab.apk"

CASES = [
    dict(name="anim", action="anim_start", expect_id="anim_text", expect="anim-done"),
    dict(name="delay40", action="delay40", expect_id="delay40_text", expect="delay40-done"),
    dict(name="delay200", action="delay200", expect_id="delay200_text", expect="delay200-done"),
    dict(name="delay1000", action="delay1000", expect_id="delay1000_text",
         expect="delay1000-done", long=True),
    dict(name="background", action="bg_start", expect_id="bg_text", expect="bg-done"),
    dict(name="chain", action="chain_start", expect_id="chain_text", expect="chain-done"),
    dict(name="dialog", action="dialog_start", expect_id="dialog_text",
         expect="dialog-open", reset="dialog_close"),
    dict(name="activity", action="activity_start", expect_id="second_text",
         expect="second-open", reset="second_finish", after_reset="anim_start"),
    dict(name="list", action="list_start", expect_id="list_status",
         expect="list-idle"),
    dict(name="progress", action="progress_start", expect_timeout=True,
         reset="progress_stop", reps=5),
    dict(name="layout", action="lt_start", expect_id="lt_text", expect="lt-added"),
    dict(name="rapid", special="rapid", expect_id="rapid_count", expect="10",
         reset="rapid_reset"),
]


def walk(nodes):
    for node in nodes or []:
        if not isinstance(node, dict):
            continue
        yield node
        yield from walk(node.get("children"))


def find_node(tree, short_id):
    for node in walk((tree or {}).get("nodes")):
        rid = node.get("resource_id") or node.get("id") or ""
        if rid == short_id or rid.endswith(":id/" + short_id):
            return node
    return None


def node_text(node):
    return "" if node is None else (node.get("text") or "")


def tree_ok(tree, case):
    node = find_node(tree, case["expect_id"])
    if node_text(node) != case["expect"]:
        return False, "text=%r" % node_text(node)
    if case.get("extra_text") and find_node(tree, case["extra_text"]) is None:
        return False, "missing %s" % case["extra_text"]
    return True, ""


def md5_file(path):
    h = hashlib.md5()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def percentiles(values, ps=(50, 95)):
    values = sorted(v for v in values if v is not None)
    if not values:
        return {("p%d" % p): None for p in ps}
    out = {}
    for p in ps:
        k = (len(values) - 1) * p / 100.0
        lo = int(k)
        hi = min(lo + 1, len(values) - 1)
        out["p%d" % p] = round(values[lo] + (values[hi] - values[lo]) * (k - lo), 2)
    return out


class Harness:
    def __init__(self, daemon, scratch, base):
        self.d = daemon
        self.scratch = scratch
        self.base = base
        self.shot = scratch / "shot.png"
        self.seq = 0
        self.history = 0

    def new_session(self, tag):
        if self.base is not None:
            try:
                self.d.call("acos_destroy", {"base": self.base})
            except Exception:  # noqa: BLE001
                pass
        self.history += 1
        self.base = {"request_id": "p11-%s-%d" % (tag, self.history),
                     "session_id": "s_p11_%s_%d" % (tag.replace("-", "_"), self.history)}
        launch = self.d.call("acos_launch", {"base": self.base, "apk": str(APK)})
        if not (launch.get("envelope") or {}).get("ok"):
            raise RuntimeError("launch failed for %s: %s" % (
                tag, launch.get("envelope")))
        time.sleep(0.08)

    def call(self, tool, **args):
        return self.d.call(tool, {"base": self.base, **args})

    def tap(self, ident, settle="strict"):
        return self.call("acos_tap", id=ident, settle=settle)

    def tree(self):
        r = self.call("acos_observe_tree", settle="fast")
        env = r.get("envelope") or {}
        return (env.get("result") or {}).get("tree") or {}, r

    def frame(self):
        self.seq += 1
        path = self.scratch / ("shot-%d.png" % self.seq)
        r = self.call("acos_observe", screenshot=str(path))
        env = r.get("envelope") or {}
        res = env.get("result") or {}
        frame = res.get("frame") or {}
        return frame.get("md5"), path, r

    def wait_text(self, case, timeout=6.0):
        deadline = time.time() + timeout
        last = ""
        while time.time() < deadline:
            tree, _ = self.tree()
            ok, why = tree_ok(tree, case)
            if ok:
                return True, why
            last = why
            time.sleep(0.03)
        return False, last


def settle_of(reply):
    env = reply.get("envelope") or {}
    result = env.get("result") or {}
    settle = result.get("settle") or {}
    if not settle and isinstance(env.get("error"), dict):
        settle = env["error"].get("details", {}).get("settle") or {}
    return env, result, settle


def do_action(h, case):
    """Return (t0, reply, wall_ms, last_fast_tap_ms)."""
    if case.get("special") == "rapid":
        reset = h.tap(case.get("reset") or "rapid_reset", settle="strict")
        time.sleep(0.08)
        for _ in range(9):
            h.tap("rapid_inc", settle="fast")
            time.sleep(0.03)
        time.sleep(0.03)
        t0 = time.time()
        reply = h.tap("rapid_inc", settle="strict")
        return t0, reply, reply["wall_ms"], None
    t0 = time.time()
    reply = h.tap(case["action"], settle="strict")
    return t0, reply, reply["wall_ms"], None


def reset_case(h, case):
    reset = case.get("reset")
    if reset:
        h.tap(reset, settle="strict")
        time.sleep(0.05)
    if case.get("after_reset"):
        deadline = time.time() + 4.0
        while time.time() < deadline:
            tree, _ = h.tree()
            if find_node(tree, case["after_reset"]) is not None:
                break
            time.sleep(0.03)
        time.sleep(0.05)


def run_case(h, case, reps):
    name = case["name"]
    print("p11: case %s (%d reps)" % (name, reps), flush=True)
    # One long-wait reference per case, in the same held session. Scrollbars
    # are disabled in the fixture, so the frame bytes for the same final state
    # are deterministic across reps.
    ref_md5 = None
    ref_note = ""
    if not case.get("expect_timeout"):
        h.new_session(name + "-ref")
        do_action(h, case)
        ok, why = h.wait_text(case, 8.0)
        if not ok:
            ref_note = "reference never reached final state: %s" % why
        time.sleep(0.12)
        ref_md5, ref_path, _ = h.frame()
        time.sleep(0.05)

    rows = []
    for i in range(reps):
        h.new_session(name + "-%d" % (i + 1))
        t0, reply, wall_ms, _ = do_action(h, case)
        env, result, settle = settle_of(reply)
        ok = bool(env.get("ok"))
        bound = settle.get("bound")
        settled = bool(settle.get("settled"))
        code = ((env.get("error") or {}).get("code") if isinstance(env.get("error"), dict)
                else None)
        row = {
            "rep": i + 1,
            "wall_ms": round(wall_ms, 2),
            "ok": ok,
            "bound": bound,
            "settled": settled,
            "mode": settle.get("mode"),
            "strict_rule": settle.get("strict_rule"),
            "reason": settle.get("reason"),
            "code": code,
            "probe_seq": result.get("probe_seq") or result.get("settle", {}).get("probe_seq"),
        }
        if case.get("expect_timeout"):
            row["expected_timeout"] = (bound == "timeout" and not settled)
            row["timeout_not_settled"] = (not settled)
            rows.append(row)
            time.sleep(0.05)
            continue
        # Immediate state check: fast tree + frame as they are right after the
        # strict reply. No settle window is allowed to hide an early reply.
        snap_t0 = time.time()
        tree, _ = h.tree()
        t_ok, t_why = tree_ok(tree, case)
        snap_md5, snap_path, _ = h.frame()
        row["snapshot_ms"] = round((time.time() - snap_t0) * 1000.0, 2)
        row["tree_ok"] = t_ok
        row["tree_note"] = t_why
        row["snapshot_md5"] = snap_md5
        row["snapshot_path"] = str(snap_path)
        row["png_ok"] = (ref_md5 is not None and snap_md5 == ref_md5)
        if case.get("long"):
            row["long_delay_early"] = not t_ok
            row["long_delay_snapshot_early"] = not row["png_ok"]
        else:
            row["false_pass"] = bool(ok and settled and not (t_ok and row["png_ok"]))
            row["early_render"] = bool(not row["png_ok"])
        rows.append(row)
        time.sleep(0.05)

    good = [r for r in rows if r.get("ok")]
    lat = [r["wall_ms"] for r in good]
    summary = {
        "n": len(rows),
        "ok": len(good),
        "latency": percentiles(lat),
    }
    if case.get("expect_timeout"):
        summary["timeouts"] = sum(1 for r in rows if r.get("expected_timeout"))
        summary["not_settled"] = sum(1 for r in rows if r.get("timeout_not_settled"))
    elif case.get("long"):
        summary["early_replies"] = sum(1 for r in rows if r.get("long_delay_early"))
        summary["snapshot_before_final"] = sum(
            1 for r in rows if r.get("long_delay_snapshot_early"))
        summary["final_reached"] = 1 if ref_md5 else 0
    else:
        summary["false_passes"] = sum(1 for r in rows if r.get("false_pass"))
        summary["early_renders"] = sum(1 for r in rows if r.get("early_render"))
        summary["tree_ok"] = sum(1 for r in rows if r.get("tree_ok"))
        summary["png_ok"] = sum(1 for r in rows if r.get("png_ok"))
    summary["reference_md5"] = ref_md5
    summary["reference_path"] = str(ref_path) if ref_md5 else None
    summary["reference_note"] = ref_note
    return {"case": case, "rows": rows, "summary": summary}

def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--acos", required=True)
    ap.add_argument("--out", required=True)
    ap.add_argument("--scratch", required=True)
    ap.add_argument("--reps", type=int, default=20)
    ap.add_argument("--cases", default="")
    ap.add_argument("--mode", default=os.environ.get("ACOS_STRICT_SETTLE", "quiet"))
    ap.add_argument("--keep-scratch", action="store_true")
    args = ap.parse_args()
    scratch = Path(args.scratch)
    if scratch.exists():
        shutil.rmtree(scratch)
    scratch.mkdir(parents=True)
    wanted = set(c for c in args.cases.split(",") if c)
    cases = [c for c in CASES if not wanted or c["name"] in wanted]
    doc = {
        "schema": "acos-p11-measure/1",
        "utc": d2_warm.utc(),
        "mode": args.mode,
        "acos": args.acos,
        "apk_sha256": benchlib.sha256_file(APK),
        "host_before": d2_warm.host_now(),
        "ok": False,
        "cases": [],
    }
    serve = daemon = None
    try:
        serve = d2_warm.Serve(args.acos, scratch)
        daemon = serve.client()
        daemon.call("ping", {})
        base = None
        h = Harness(daemon, scratch, base)
        # One fresh session per rep: the strict engine's pixel-change guard
        # compares against the previous capture, and repeating an action that
        # ends in the same final pixels would otherwise be a documented
        # `pixels-unchanged` timeout rather than the case's settle cost.
        for case in cases:
            reps = case.get("reps", args.reps)
            # The shared ART lock makes 20 reps of every slow animation case
            # expensive; event mode uses 10 for those, and 5 for rapid where
            # the fixture's 30 ms fast-tap setup is also under test.
            if args.mode == "event" and case["name"] in {
                    "background", "chain", "activity", "list", "layout"}:
                reps = min(reps, 10)
            if args.mode == "event" and case["name"] == "rapid":
                reps = min(reps, 5)
            doc["cases"].append(run_case(h, case, reps))
        if h.base is not None:
            try:
                daemon.call("acos_destroy", {"base": h.base})
            except Exception:  # noqa: BLE001
                pass
        doc["ok"] = True
    except Exception as exc:  # noqa: BLE001
        doc["error"] = "%s: %s" % (type(exc).__name__, exc)
    finally:
        if daemon is not None:
            try:
                if 'h' in locals() and h.base is not None:
                    daemon.call("acos_destroy", {"base": h.base})
            except Exception:  # noqa: BLE001
                pass
            try:
                daemon.close()
            except Exception:  # noqa: BLE001
                pass
        if serve is not None:
            doc["serve_rc"] = serve.close()
        time.sleep(0.5)
        doc["survivors"] = d2_warm.survivors(scratch)
        doc["host_after"] = d2_warm.host_now()
        doc["contaminated"] = not (d2_warm.host_ok(doc["host_before"])
                                   and d2_warm.host_ok(doc["host_after"]))
        doc["utc_finished"] = d2_warm.utc()
        Path(args.out).parent.mkdir(parents=True, exist_ok=True)
        with open(args.out, "w") as f:
            json.dump(doc, f, indent=2)
        print("p11: wrote %s ok=%s mode=%s" % (args.out, doc.get("ok"), args.mode),
              flush=True)
        for c in doc.get("cases", []):
            print("  %-12s %s" % (c["case"]["name"], c["summary"]), flush=True)
        if not args.keep_scratch:
            shutil.rmtree(scratch, ignore_errors=True)
    return 0 if doc.get("ok") else 1


if __name__ == "__main__":
    sys.exit(main())
