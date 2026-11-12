#!/usr/bin/env python3
"""Z7 lifetime-matched memory: whole-system Pss at a fixed age of EACH child.

The Z6 protocol sampled 0.5/15/60 s after the *last tap*, and the tap phase grows
with N, so its "0.5 s" cell was really 3-65 s after the children's first frames.
Z7 anchors on the guest's own `ready` (the first live frame) instead:

  * every session's ready line is read before any tree/tap/screenshot request, so
    `t_ready` is the guest's first-frame time, not a driver artefact;
  * for each child k and each age A the driver takes one whole-tree snapshot at
    `t_ready[k] + A`; the child named by that event is exactly A seconds old, and
    the snapshot records every other child's actual age too;
  * the cell reports the median of the N snapshots and the age range, so a cell
    can never hide that sessions were sampled at different ages.

After the 60 s snapshot the run proves the sessions are still correct and
isolated (counter: 0 -> tap -> 1; every untapped session still 0; pixels).

Usage:
  lifetime.py --arm c --n 10 --out DIR --json FILE [--ages 0.5,2,5,15,60]
              [--app counter|form-validation]
"""
import argparse
import json
import os
import statistics
import sys
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import common as C  # noqa: E402
import drive as z4  # noqa: E402


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--arm", required=True, choices=C.ARMS)
    ap.add_argument("--n", type=int, required=True)
    ap.add_argument("--out", required=True)
    ap.add_argument("--json", required=True)
    ap.add_argument("--label", default="")
    ap.add_argument("--ages", default="0.5,2,5,15,60")
    ap.add_argument("--app", default="counter", choices=["counter", "form-validation"])
    args = ap.parse_args()
    ages = [float(x) for x in args.ages.split(",") if x.strip()]
    profile = C.app_profile(args.app)
    root = os.path.abspath(args.out)
    plan = os.path.join(root, "sessions")
    report = {
        "schema": "z7-lifetime/1",
        "arm": args.arm, "mode": C.MODES[args.arm], "n": args.n,
        "app": args.app, "ages_s": ages, "label": args.label,
        "started_utc": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
        "art_bin": C.ART, "zygote_build": C.ZYGOTE_BUILD,
        "failures": [], "sessions": {},
    }
    procs, sessions, t0, overlay = C.spawn_cohort(args.arm, args.n, root, plan, args.app)
    report["knobs"] = overlay
    report["spawn_ms"] = round((time.perf_counter() - t0) * 1000.0, 1)
    try:
        ready = C.read_all_ready(sessions, t0)
        report["all_ready_ms"] = round((time.perf_counter() - t0) * 1000.0, 1)
        t_ready = {k: v["t_ready_ms"] / 1000.0 for k, v in ready.items()}
        report["sessions_ready_ms"] = {str(k): v["t_ready_ms"] for k, v in ready.items()}
        for k, v in ready.items():
            report["sessions"][str(k)] = {
                "ready_ms": v["t_ready_ms"], "ready_ok": bool(v["reply"].get("ok")),
                "frame_seq": v["reply"].get("frame_seq"),
            }
            if not v["reply"].get("ok"):
                report["failures"].append(f"session {k}: ready not ok: {v['reply']}")
        if report["failures"]:
            raise SystemExit(0)

        # Events: (target_age, child). At each event one whole-tree snapshot.
        events = sorted((t_ready[k] + a, a, k) for a in ages for k in t_ready)
        samples = []
        started = time.perf_counter()
        for target, age, child in events:
            wait = target - (time.perf_counter() - started)
            if wait > 0:
                time.sleep(wait)
            snap = z4.sample_memory(os.getpid(), f"age{age:g}-child{child}")
            actual = (time.perf_counter() - started)
            snap["target_age_s"] = age
            snap["ancestor_child"] = child
            snap["snapshot_s"] = round(actual, 3)
            snap["child_ages_s"] = {
                str(k): round(actual - t_ready[k], 3) for k in sorted(t_ready)
            }
            samples.append(snap)
            print(f"[{args.arm}-n{args.n}-{args.app}] age={age:g} child={child} "
                  f"at={actual:.2f}s pss={snap['total']['pss']/1024:.1f}MiB "
                  f"pd={snap['total']['private_dirty']/1024:.1f}MiB "
                  f"swappss={snap['total']['swap_pss']/1024:.3f}", flush=True)
        report["samples"] = samples

        # Cell summary: per age, median over the N child-anchored snapshots.
        cells = {}
        for a in ages:
            rows = [s for s in samples if s["target_age_s"] == a]
            pss = [s["total"]["pss"] / 1024.0 for s in rows]
            pd = [s["total"]["private_dirty"] / 1024.0 for s in rows]
            sw = [s["total"]["swap_pss"] / 1024.0 for s in rows]
            ages_seen = [v for s in rows for v in s["child_ages_s"].values()]
            cells[f"{a:g}"] = {
                "n": len(rows),
                "pss_median_mib": round(statistics.median(pss), 1),
                "pss_min_mib": round(min(pss), 1), "pss_max_mib": round(max(pss), 1),
                "private_dirty_median_mib": round(statistics.median(pd), 1),
                "swap_pss_max_mib": round(max(sw), 3),
                "child_age_min_s": round(min(ages_seen), 3),
                "child_age_max_s": round(max(ages_seen), 3),
            }
        report["cells"] = cells

        # Correctness: every session had its first frame; now one real task each,
        # with the ordered isolation matrix (same rule as Z4/Z6).
        counts = {}
        for k in sorted(sessions):
            tree, _, _ = sessions[k].call({"cmd": "tree"})
            node = C.by_suffix(tree.get("tree"), profile["assert_suffix"])
            counts[k] = C.node_text(node)
        if profile["kind"] == "tap":
            bad = {k: v for k, v in counts.items() if v.strip() != "0"}
            if bad:
                report["failures"].append(f"initial counts {bad}, expected 0")
        for k in sorted(sessions):
            task_rec = {}
            ok, rec = C.verify_task(sessions[k], profile, k, task_rec)
            report["sessions"][str(k)]["task"] = rec
            if not ok:
                report["failures"].append(f"session {k}: task not verified: {rec}")
            for j in range(k + 1, args.n):
                tree, _, _ = sessions[j].call({"cmd": "tree"})
                node = C.by_suffix(tree.get("tree"), profile["assert_suffix"])
                got = C.node_text(node)
                expected = "0" if profile["kind"] == "tap" else counts[j]
                if profile["kind"] == "tap" and got.strip() != expected:
                    report["failures"].append(
                        f"session {j}: count {got!r} after session {k}'s tap")
        md5s = [report["sessions"][str(k)].get("task", {}).get("screenshot_md5")
                for k in sorted(sessions)]
        report["screenshot_md5_all_equal"] = len(set(md5s)) == 1
        report["total_ms"] = round((time.perf_counter() - t0) * 1000.0, 1)
    finally:
        report["launcher_exit"] = C.close_cohort(sessions, procs, bound=20.0)
        zpid = C.zygote_pid(plan)
        report["zygote_pid"] = zpid
        report["zygote_alive_after_close"] = (
            bool(zpid) and os.path.exists(f"/proc/{zpid}"))
        report["verdict"] = "pass" if not report["failures"] else "fail"
        C.write_json(args.json, report)
        print(f"[{args.arm}-n{args.n}-{args.app}] verdict={report['verdict']} "
              f"failures={len(report['failures'])} -> {args.json}", flush=True)


if __name__ == "__main__":
    main()
