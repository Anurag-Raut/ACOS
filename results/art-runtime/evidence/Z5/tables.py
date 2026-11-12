#!/usr/bin/env python3
"""Print the Z5 gate markdown tables from the drive.py JSONs.

    python3 tables.py [root]    # root defaults to /home/Qui/Projects/acos/.cache/z5-run

Arm (a) uses the warm repeat (`a-N-r2.json`) when present: the first arm-(a) run of the session
paid a one-off dex build (see Z5 §1), which inflates only its ready time and CPU, not its memory.
"""
import json
import os
import sys

MIB = 1024.0
STUB_Z4 = 9.1  # MiB Pss per session, measured by Z4 (zygote-5.json, 4 stubs = 36.4 MiB)


def load(root, arm, n):
    for name in (f"{arm}-{n}-r2.json", f"{arm}-{n}.json"):
        p = os.path.join(root, name)
        if os.path.exists(p):
            return json.load(open(p)), os.path.basename(p)
    return None, None


def row(d):
    m = d["memory"]
    r = m["sum_by_role"]
    drv = r.get("driver", {})
    whole = m["total"]["pss"] / MIB - drv.get("pss", 0) / MIB
    whole_pd = m["total"]["private_dirty"] / MIB - drv.get("private_dirty", 0) / MIB
    return {
        "n": d["n"], "verdict": d.get("verdict"),
        "whole": whole, "whole_pd": whole_pd,
        "guest": m["guest_side"]["pss"] / MIB,
        "guest_pd": m["guest_side"]["private_dirty"] / MIB,
        "launcher": r.get("launcher", {}).get("pss", 0) / MIB,
        "bwrap": r.get("bwrap", {}).get("pss", 0) / MIB,
        "stub": r.get("stub", {}).get("pss", 0) / MIB,
        "first": d.get("first_session_ready_ms"),
        "per_extra": d.get("per_extra_session_ready_ms"),
        "cpu": d.get("cpu_bringup_seconds_no_driver"),
        "all_ready": d.get("all_ready_ms"),
    }


def main():
    root = sys.argv[1] if len(sys.argv) > 1 else "/home/Qui/Projects/acos/.cache/z5-run"
    ns = [int(x) for x in (os.environ.get("Z5_SUMMARY_NS") or "1 5 10 20").split()]
    rows, src = {}, {}
    for arm in "abc":
        for n in ns:
            d, name = load(root, arm, n)
            if d:
                rows[(arm, n)] = row(d)
                src[(arm, n)] = name

    def f(v, p=1):
        return "—" if v is None else f"{v:.{p}f}"

    print("### Per-N, MiB (whole system = driver tree − driver)\n")
    print("| N | arm | ΣPss | ΣPrivate_Dirty | guest side ΣPss | launchers | bwrap | holders |"
          " verdict |")
    print("|---:|---|---:|---:|---:|---:|---:|---:|---|")
    for n in ns:
        for arm in "abc":
            if (arm, n) in rows:
                r = rows[(arm, n)]
                print(f"| {n} | ({arm}) | {f(r['whole'])} | {f(r['whole_pd'])} | {f(r['guest'])} |"
                      f" {f(r['launcher'])} | {f(r['bwrap'])} | {f(r['stub'])} | {r['verdict']} |")
            else:
                print(f"| {n} | ({arm}) | — | — | — | — | — | — | not measured |")

    print("\n### Marginals, MiB per extra session\n")
    print("| arm | ΣPss/session (least squares, N=1…20) | ΣPss/session (endpoints) |"
          " guest-side ΣPss/session |")
    print("|---|---:|---:|---:|")
    for arm in "abc":
        pts = [(n, rows[(arm, n)]["whole"]) for n in ns if (arm, n) in rows]
        gp = [(n, rows[(arm, n)]["guest"]) for n in ns if (arm, n) in rows]
        if len(pts) < 2:
            continue
        mx = sum(p[0] for p in pts) / len(pts)
        my = sum(p[1] for p in pts) / len(pts)
        s = (sum((p[0] - mx) * (p[1] - my) for p in pts)
             / sum((p[0] - mx) ** 2 for p in pts))
        ep = (pts[-1][1] - pts[0][1]) / (pts[-1][0] - pts[0][0])
        gmx = sum(p[0] for p in gp) / len(gp)
        gmy = sum(p[1] for p in gp) / len(gp)
        gs = (sum((p[0] - gmx) * (p[1] - gmy) for p in gp)
              / sum((p[0] - gmx) ** 2 for p in gp))
        print(f"| ({arm}) | {f(s)} | {f(ep)} | {f(gs)} |")

    print("\n### Savings at the same N (%)\n")
    print("| N | (b) vs (a) whole | (c) vs (a) whole | (c) vs (a)+(b) whole |"
          " (c) vs (a) guest | (c) vs (a) private dirty |")
    print("|---:|---:|---:|---:|---:|---:|")
    for n in ns:
        if not all((arm, n) in rows for arm in "abc"):
            continue
        a, b, c = rows[("a", n)], rows[("b", n)], rows[("c", n)]
        pc = lambda x, y: (x - y) / x * 100.0  # noqa: E731
        print(f"| {n} | {f(pc(a['whole'], b['whole']))} | {f(pc(a['whole'], c['whole']))} |"
              f" {f(pc(b['whole'], c['whole']))} | {f(pc(a['guest'], c['guest']))} |"
              f" {f(pc(a['whole_pd'], c['whole_pd']))} |")

    print("\n### Timing and CPU\n")
    print("| N | arm | first session ready (ms) | per extra session ready (ms) |"
          " all N ready (ms) | host CPU-s to bring up N |")
    print("|---:|---|---:|---:|---:|---:|")
    for n in ns:
        for arm in "abc":
            if (arm, n) in rows:
                r = rows[(arm, n)]
                print(f"| {n} | ({arm}) | {f(r['first'], 0)} | {f(r['per_extra'], 0)} |"
                      f" {f(r['all_ready'], 0)} | {f(r['cpu'], 2)} |")
            else:
                print(f"| {n} | ({arm}) | — | — | — | — |")

    print("\n### Reconstructed: arm (c) with Z4's 9.1 MiB tombstone instead of the holder\n")
    print("| N | (c) measured ΣPss | (c) + 9.1×(N−1) ΣPss | vs (a) % | vs (a)+(b) % |")
    print("|---:|---:|---:|---:|---:|")
    for n in ns:
        if not all((arm, n) in rows for arm in "abc"):
            print(f"| {n} | not measured | — | — | — |")
            continue
        a, b, c = rows[("a", n)], rows[("b", n)], rows[("c", n)]
        cz = c["whole"] + STUB_Z4 * (n - 1)
        print(f"| {n} | {f(c['whole'])} | {f(cz)} | {(a['whole'] - cz) / a['whole'] * 100:.1f} |"
              f" {(b['whole'] - cz) / b['whole'] * 100:.1f} |")
    print("\n### Steady state (60 s settle), arms (a) and (c)\n")
    print("| N | (a) ΣPss | (a) ΣPD | (c) ΣPss | (c) ΣPD | saving | (c) vs (b) |")
    print("|---:|---:|---:|---:|---:|---:|---:|")
    for n in ns:
        pa = os.path.join(root, f"a-{n}-s60.json")
        pc = os.path.join(root, f"c-{n}-s60.json")
        if not (os.path.exists(pa) and os.path.exists(pc)):
            continue
        a = row(json.load(open(pa)))
        c = row(json.load(open(pc)))
        b = rows.get(("b", n), {}).get("whole")
        vsb = f"{(b - c['whole']) / b * 100:.1f}" if b else "—"
        print(f"| {n} | {f(a['whole'])} | {f(a['whole_pd'])} | {f(c['whole'])} |"
              f" {f(c['whole_pd'])} | {(a['whole'] - c['whole']) / a['whole'] * 100:.1f} | {vsb} |")

    print("\nsources: " + ", ".join(f"{k[0]}-{k[1]}={v}" for k, v in sorted(src.items())))


if __name__ == "__main__":
    main()
