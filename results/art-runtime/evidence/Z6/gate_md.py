#!/usr/bin/env python3
"""Z6: emit the markdown blocks that go into results/art-runtime/Z6-gate.md.

Every number in the report's tables is produced here from the driver reports, so
the prose cannot drift from the JSON (and no number is ever typed by hand).
"""
import glob
import json
import os
import sys

DEFAULT_GLOB = "/home/Qui/Projects/acos/.cache/z6-run/*.json"
MB = 1024.0
TIMES = (0.5, 15.0, 60.0)
ARMS = ("a", "b", "c")


def load(paths):
    runs = {}
    for p in paths:
        try:
            d = json.load(open(p))
        except (OSError, json.JSONDecodeError):
            continue
        if d.get("arm") in ARMS and isinstance(d.get("n"), int):
            runs[(d["arm"], d["n"])] = d
    return runs


def sample(d, t):
    for s in d.get("memory_samples") or []:
        if abs(s.get("t_s", -1) - t) < 1e-6:
            return s
    return None


def rows(s):
    return [r for r in s["processes"] if r["role"] != "driver"]


def pss(s):
    return sum(r["pss"] for r in rows(s)) / MB


def pd(s):
    return sum(r["private_dirty"] for r in rows(s)) / MB


def swap_pss(s):
    return sum(r.get("swap_pss", 0) for r in rows(s)) / MB


def role(s, *names):
    return sum(r["pss"] for r in rows(s) if r["role"] in names) / MB


def kids(s):
    rs = [r for r in rows(s)
          if r["role"] == "guest" or r["comm"].startswith(("fixtures", "ixtures"))]
    if not rs:
        return None
    return {"n": len(rs), "pss": sum(r["pss"] for r in rs) / MB,
            "pd": sum(r["private_dirty"] for r in rs) / MB,
            "mean": sum(r["pss"] for r in rs) / len(rs) / MB,
            "pd_mean": sum(r["private_dirty"] for r in rs) / len(rs) / MB}


def ls(xs, ys):
    k = len(xs)
    if k < 2:
        return None, None
    mx, my = sum(xs) / k, sum(ys) / k
    den = sum((x - mx) ** 2 for x in xs)
    if den == 0:
        return None, None
    m = sum((x - mx) * (y - my) for x, y in zip(xs, ys)) / den
    return m, my - m * mx


def main():
    runs = load(sys.argv[1:] or sorted(glob.glob(DEFAULT_GLOB)))
    ns = sorted({n for (a, n) in runs})
    o = []
    w = o.append

    # ---- gate table: ΣPss per arm/N/t
    w("### ΣPss, MiB, whole system (every process except the driver)\n")
    hdr = "| arm | N |" + "".join(f" {t:g} s | ΣPD {t:g} s |" for t in TIMES) + " verdict |"
    w(hdr)
    w("|---|---|" + "---:|---:|" * len(TIMES) + "---|")
    for arm in ARMS:
        for n in ns:
            d = runs.get((arm, n))
            if not d:
                continue
            cells = []
            for t in TIMES:
                s = sample(d, t)
                cells.append(f" — | — |" if not s else f" {pss(s):.1f} | {pd(s):.1f} |")
            w(f"| ({arm}) | {n} |" + "".join(cells) + f" {d.get('verdict')} |")
    w("")

    # ---- savings
    for t in TIMES:
        w(f"### Saving at the same N, t = {t:g} s (positive = the arm uses less than the "
          f"comparison)\n")
        w("| N | (a) ΣPss | (b) ΣPss | (c) ΣPss | (c) vs (a) | (c) vs (b) | (b) vs (a) |")
        w("|---:|---:|---:|---:|---:|---:|---:|")
        for n in ns:
            sa = sample(runs[("a", n)], t) if ("a", n) in runs else None
            sb = sample(runs[("b", n)], t) if ("b", n) in runs else None
            sc = sample(runs[("c", n)], t) if ("c", n) in runs else None
            if not sa or not sc:
                continue
            pa = pss(sa)
            pb = pss(sb) if sb else None
            pc = pss(sc)
            cab = f"{(pa - pc) / pa * 100:+.1f} %"
            cbb = f"{(pb - pc) / pb * 100:+.1f} %" if pb else "—"
            bba = f"{(pa - pb) / pa * 100:+.1f} %" if pb else "—"
            w(f"| {n} | {pa:.1f} | {pb:.1f} | {pc:.1f} | {cab} | {cbb} | {bba} |"
              if pb else f"| {n} | {pa:.1f} | — | {pc:.1f} | {cab} | — | — |")
        w("")

    # ---- marginals
    w("### Marginal whole-system ΣPss per extra session (least squares over N = 1/5/10/20), "
      "MiB — the fit's intercept is the fixed shared cost\n")
    w("| arm | t | marginal | fixed cost | guest side / session | child PD / session |")
    w("|---|---|---:|---:|---:|---:|")
    for arm in ARMS:
        for t in TIMES:
            xs, ys, gs, ps = [], [], [], []
            for n in ns:
                d = runs.get((arm, n))
                s = sample(d, t) if d else None
                if not s:
                    continue
                k = kids(s)
                xs.append(n)
                ys.append(pss(s))
                gs.append(k["pss"] if k else 0.0)
                ps.append(k["pd"] if k else 0.0)
            if len(xs) < 2:
                continue
            m, b = ls(xs, ys)
            gm, _ = ls(xs, gs)
            pm, _ = ls(xs, ps)
            w(f"| ({arm}) | {t:g} s | {m:.1f} | {b:.1f} | {gm:.1f} | {pm:.1f} |")
    w("")

    # ---- per child
    w("### Per-session app child (mean over children), Pss / Private_Dirty, MiB\n")
    w("| cell | 0.5 s | 15 s | 60 s |")
    w("|---|---:|---:|---:|")
    for arm in ARMS:
        for n in ns:
            d = runs.get((arm, n))
            if not d:
                continue
            cells = []
            for t in TIMES:
                s = sample(d, t)
                k = kids(s) if s else None
                cells.append(f"{k['mean']:.0f} / {k['pd_mean']:.0f}" if k else "—")
            w(f"| ({arm}) N={n} | " + " | ".join(cells) + " |")
    w("")

    # ---- composition
    w("### Composition at 60 s, MiB Pss\n")
    w("| arm | N | app children | zygote stack | launchers | bwrap | holders | total |")
    w("|---|---|---:|---:|---:|---:|---:|---:|")
    for arm in ARMS:
        for n in ns:
            d = runs.get((arm, n))
            s = sample(d, 60.0) if d else None
            if not s:
                continue
            k = kids(s)
            zyg = role(s, "zygote")
            ch = k["pss"] if k else 0.0
            stack = (zyg - ch) if zyg else 0.0
            w(f"| ({arm}) | {n} | {ch:.1f} | {stack:.1f} | {role(s, 'launcher'):.1f} | "
              f"{role(s, 'bwrap'):.1f} | {role(s, 'stub'):.1f} | {pss(s):.1f} |")
    w("")

    # ---- startup / cpu
    w("### Startup and host CPU\n")
    w("| arm | N | first session ready (ms) | per extra (ms) | all N ready (ms) | "
      "CPU-s | CPU-s excl driver |")
    w("|---|---|---:|---:|---:|---:|---:|")
    for arm in ARMS:
        for n in ns:
            d = runs.get((arm, n))
            if not d:
                continue
            f = d.get("first_session_ready_ms")
            e = d.get("per_extra_session_ready_ms")
            w(f"| ({arm}) | {n} | {fmt(f)} | {fmt(e)} | {fmt(d.get('all_ready_ms'))} | "
              f"{fmt(d.get('cpu_bringup_seconds'), 2)} | "
              f"{fmt(d.get('cpu_bringup_seconds_no_driver'), 2)} |")
    w("")

    # ---- swap
    w("### Swap\n")
    w("| arm | N | ΣSwapPss | ΣSwap | scope memory.swap.max | memory.swap.current |")
    w("|---|---|---:|---:|---|---|")
    for arm in ARMS:
        for n in ns:
            d = runs.get((arm, n))
            s = sample(d, 60.0) if d else None
            if not s:
                continue
            cg = s.get("cgroups") or {}
            w(f"| ({arm}) | {n} | {swap_pss(s):.3f} | "
              f"{sum(r.get('swap', 0) for r in rows(s)) / MB:.3f} | "
              f"{sorted({v.get('memory.swap.max') for v in cg.values()})} | "
              f"{sorted({v.get('memory.swap.current') for v in cg.values()})} |")
    w("")
    print("\n".join(o))


def fmt(x, nd=0):
    if x is None:
        return "—"
    return f"{x:.{nd}f}"


if __name__ == "__main__":
    main()
