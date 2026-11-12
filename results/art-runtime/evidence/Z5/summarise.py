#!/usr/bin/env python3
"""Z5 gate summary: turn the three arms' drive.py JSON into the gate table.

Whole system = every process in the driver's tree *except* the driver itself
(launchers + bwrap + guests/zygote + sandbox helpers + per-session holders),
which is the brief's definition. `with_driver` is printed too so Z4's numbers
(which included the driver) stay comparable.

    python3 summarise.py /home/Qui/Projects/acos/.cache/z5-run
"""
import json
import os
import sys

MIB = 1024.0


def load(root, arm, n):
    for name in (f"{arm}-{n}.json", f"{arm}-{n}-warm.json", f"{arm}-{n}-r2.json"):
        p = os.path.join(root, name)
        if os.path.exists(p):
            return json.load(open(p))
    return None


def role_pss(mem, role):
    return mem["sum_by_role"].get(role, {}).get("pss", 0) / MIB


def row(d):
    m = d["memory"]
    t = m["total"]
    driver = role_pss(m, "driver")
    whole = t["pss"] / MIB - driver
    whole_pd = t["private_dirty"] / MIB - m["sum_by_role"].get(
        "driver", {}).get("private_dirty", 0) / MIB
    stub = role_pss(m, "stub")
    guest = m["guest_side"]["pss"] / MIB
    launcher = role_pss(m, "launcher")
    bwrap = role_pss(m, "bwrap")
    return {
        "arm": d.get("arm", "?"), "mode": d["mode"], "n": d["n"],
        "verdict": d.get("verdict"),
        "knobs": d.get("knobs", {}),
        "sessions": len(d.get("sessions", {})),
        "whole_pss": round(whole, 1),
        "whole_pss_with_driver": round(t["pss"] / MIB, 1),
        "whole_pd": round(whole_pd, 1),
        "guest_side_pss": round(guest, 1),
        "guest_side_pd": round(m["guest_side"]["private_dirty"] / MIB, 1),
        "launcher_pss": round(launcher, 1),
        "bwrap_pss": round(bwrap, 1),
        "stub_pss": round(stub, 1),
        "stub_per_session": round(stub / max(1, d["n"] - 1) if d["mode"] == "zygote" else 0, 2),
        "first_ready_ms": d.get("first_session_ready_ms"),
        "per_extra_ready_ms": d.get("per_extra_session_ready_ms"),
        "all_ready_ms": d.get("all_ready_ms"),
        "cpu_bringup_s": d.get("cpu_bringup_seconds"),
        "cpu_bringup_s_no_driver": d.get("cpu_bringup_seconds_no_driver"),
        "md5_initial_all_equal": d.get("md5_initial_all_equal"),
        "md5_after_tap_all_equal": d.get("md5_after_tap_all_equal"),
        "failures": len(d.get("failures") or []),
        "fork_to_ready_ms": d.get("fork_to_ready_ms"),
    }


def slope(points):
    """Least-squares MiB/session over (n, value) points."""
    n = len(points)
    sx = sum(p[0] for p in points)
    sy = sum(p[1] for p in points)
    sxx = sum(p[0] * p[0] for p in points)
    sxy = sum(p[0] * p[1] for p in points)
    den = n * sxx - sx * sx
    return (n * sxy - sx * sy) / den if den else None


def main():
    root = sys.argv[1] if len(sys.argv) > 1 else "/home/Qui/Projects/acos/.cache/z5-run"
    ns = [int(x) for x in (os.environ.get("Z5_SUMMARY_NS") or "1 5 10 20").split()]
    arms = os.environ.get("Z5_SUMMARY_ARMS", "a b c").split()
    rows = {}
    for arm in arms:
        for n in ns:
            d = load(root, arm, n)
            if d is None:
                continue
            rows[(arm, n)] = row(d)
    out = {"rows": [rows[k] for k in sorted(rows, key=lambda k: (k[0], k[1]))]}
    # marginals + savings
    for arm in arms:
        pts = [(n, rows[(arm, n)]["whole_pss"]) for n in ns if (arm, n) in rows]
        if len(pts) >= 2:
            out.setdefault("slope_whole", {})[arm] = round(slope(pts), 2)
            out.setdefault("slope_whole_endpoints", {})[arm] = round(
                (pts[-1][1] - pts[0][1]) / (pts[-1][0] - pts[0][0]), 2)
        gp = [(n, rows[(arm, n)]["guest_side_pss"]) for n in ns if (arm, n) in rows]
        if len(gp) >= 2:
            out.setdefault("slope_guest", {})[arm] = round(slope(gp), 2)
    for base in ("a",):
        for n in ns:
            if (base, n) not in rows:
                continue
            for arm in arms:
                if arm == base or (arm, n) not in rows:
                    continue
                b = rows[(base, n)]["whole_pss"]
                v = rows[(arm, n)]["whole_pss"]
                out.setdefault("saving_vs_a_whole", {}).setdefault(arm, {})[n] = round(
                    (b - v) / b * 100.0, 1)
                gb = rows[(base, n)]["guest_side_pss"]
                gv = rows[(arm, n)]["guest_side_pss"]
                out.setdefault("saving_vs_a_guest", {}).setdefault(arm, {})[n] = round(
                    (gb - gv) / gb * 100.0, 1)
                if ("b", n) in rows:
                    ab = rows[("b", n)]["whole_pss"]
                    out.setdefault("saving_vs_ab_whole", {}).setdefault(arm, {})[n] = round(
                        (ab - v) / ab * 100.0, 1)
    print(json.dumps(out, indent=1, sort_keys=True))
    with open(os.path.join(root, "summary.json"), "w") as f:
        json.dump(out, f, indent=1, sort_keys=True)


if __name__ == "__main__":
    main()
