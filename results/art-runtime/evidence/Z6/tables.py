#!/usr/bin/env python3
"""Z6: turn the per-configuration driver reports into the gate tables.

Input:  $Z6_OUT/*.json (or arguments) — `drive.py` reports written by run-z6.sh
Output: table.txt on stdout, summary.json next to this script.

Definitions (identical to Z5-gate.md so the two reports are comparable):
  * whole system = every process in the driver's tree EXCEPT the driver;
  * ΣPss = Σ Pss from smaps_rollup (shared pages apportioned), ΣPD = Σ Private_Dirty;
  * marginal = least-squares slope of ΣPss over N = 1/5/10/20 (MiB per extra session);
    the fit's intercept is the fixed shared cost;
  * saving = (a − x)/a at the same N and the same time point; positive = x uses less.
"""
import glob
import json
import os
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
DEFAULT_GLOB = "/home/Qui/Projects/acos/.cache/z6-run/*.json"
ARMS = ["a", "b", "c"]
ARM_NAME = {
    "a": "(a) optimised current (main default)",
    "b": "(b) (a) + B3 shared boot image",
    "c": "(c) zygote + ACOS_ZYGOTE_NO_STUB=1",
    "d": "(d) (a) + ACOS_ART_HEAPGROWTHLIMIT (control, not a gate arm)",
}
MB = 1024.0


def app_child(row):
    return row["role"] in ("guest",) or row["comm"].startswith(("fixtures", "ixtures"))


def load(paths):
    runs = {}
    for p in paths:
        try:
            d = json.load(open(p))
        except (OSError, json.JSONDecodeError):
            continue
        arm, n = d.get("arm") or "?", d.get("n")
        if arm not in ARMS or not isinstance(n, int):
            continue
        runs[(arm, n)] = d
    return runs


def sample_of(d, t):
    for s in d.get("memory_samples") or []:
        if abs(s.get("t_s", -1) - t) < 1e-6:
            return s
    return None


def whole(sample, roles=None):
    rows = [r for r in sample["processes"] if r["role"] != "driver"]
    if roles:
        rows = [r for r in rows if r["role"] in roles]
    return {
        "n": len(rows),
        "pss": sum(r["pss"] for r in rows) / MB,
        "pd": sum(r["private_dirty"] for r in rows) / MB,
        "swap_pss": sum(r.get("swap_pss", 0) for r in rows) / MB,
        "rss": sum(r["rss"] for r in rows) / MB,
    }


def children(sample):
    rows = [r for r in sample["processes"] if r["role"] != "driver" and app_child(r)]
    if not rows:
        return None
    return {
        "n": len(rows),
        "pss_mean": sum(r["pss"] for r in rows) / len(rows) / MB,
        "pd_mean": sum(r["private_dirty"] for r in rows) / len(rows) / MB,
        "swap_pss": sum(r.get("swap_pss", 0) for r in rows) / MB,
    }


def ls_slope(ns, ys):
    """Least-squares slope/intercept of y over n."""
    k = len(ns)
    if k < 2:
        return None, None
    mx = sum(ns) / k
    my = sum(ys) / k
    den = sum((x - mx) ** 2 for x in ns)
    if den == 0:
        return None, None
    slope = sum((x - mx) * (y - my) for x, y in zip(ns, ys)) / den
    return slope, my - slope * mx


def fmt(x, spec="%.1f"):
    return "—" if x is None else spec % x


def main():
    paths = sys.argv[1:] or sorted(glob.glob(DEFAULT_GLOB))
    runs = load(paths)
    ns = sorted({n for (a, n) in runs})
    times = sorted({t for d in runs.values() for t in (d.get("sample_times_s") or [])})
    out = []
    w = out.append

    w("Z6 gate tables — whole system = every process of the driver's tree except the driver")
    w("configurations: " + ", ".join(f"{a}-n{n}" for (a, n) in sorted(runs)))
    w("time points (s after the last tap): " + ", ".join(f"{t:g}" for t in times))
    w("")

    # ---------------------------------------------------------------- per N/t
    for t in times:
        w(f"== ΣPss / ΣPrivate_Dirty, MiB, t = {t:g}s ==")
        w("%-3s %-3s %9s %9s %9s %9s %9s %9s %7s %s"
          % ("arm", "N", "ΣPss", "ΣPD", "children", "launchers", "other", "SwapPss",
             "procs", "verdict"))
        for arm in ARMS:
            for n in ns:
                d = runs.get((arm, n))
                if not d:
                    continue
                s = sample_of(d, t)
                if not s:
                    continue
                allw = whole(s)
                ch = children(s)
                lw = whole(s, {"launcher"})
                ch_pss = (ch["pss_mean"] * ch["n"]) if ch else 0.0
                other = allw["pss"] - ch_pss - lw["pss"]
                w("%-3s %-3d %9.1f %9.1f %9.1f %9.1f %9.1f %9.3f %7d %s"
                  % (arm, n, allw["pss"], allw["pd"], ch_pss,
                     lw["pss"], other, allw["swap_pss"], allw["n"],
                     d.get("verdict")))
        w("")

    # --------------------------------------------------------- per child detail
    w("== per-session app child (mean over children): Pss / Private_Dirty, MiB ==")
    w("%-24s %s" % ("cell", "  ".join(f"t={t:g}s" for t in times)))
    for arm in ARMS:
        for n in ns:
            d = runs.get((arm, n))
            if not d:
                continue
            cells = []
            for t in times:
                s = sample_of(d, t)
                ch = children(s) if s else None
                cells.append(f"{ch['pss_mean']:.0f}/{ch['pd_mean']:.0f}" if ch else "—")
            w("%-24s %s" % (f"({arm}) N={n}", "     ".join(cells)))
    w("")

    # -------------------------------------------------------------- marginals
    w("== marginal whole-system ΣPss per extra session (least squares over N), MiB ==")
    w("   whole = all managed processes; guest-side = app children only; fixed = the fit's "
      "intercept (the shared core)")
    w("%-3s %-6s %10s %10s %12s %10s" % ("arm", "t", "whole/sess", "fixed",
                                         "guest-side/sess", "child PD/sess"))
    for arm in ARMS:
        for t in times:
            xs, ys, cs, cps = [], [], [], []
            for n in ns:
                d = runs.get((arm, n))
                s = sample_of(d, t) if d else None
                if not s:
                    continue
                xs.append(n)
                ys.append(whole(s)["pss"])
                rows = [r for r in s["processes"]
                        if r["role"] != "driver" and app_child(r)]
                cs.append(sum(r["pss"] for r in rows) / MB)
                cps.append(sum(r["private_dirty"] for r in rows) / MB)
            if len(xs) < 2:
                continue
            slope, icept = ls_slope(xs, ys)
            cslope, _ = ls_slope(xs, cs)
            pdslope, _ = ls_slope(xs, cps)
            w("%-3s %-6g %10.1f %10.1f %12.1f %10.1f"
              % (arm, t, slope, icept, cslope, pdslope))
    w("")

    # ---------------------------------------------------------------- savings
    for t in times:
        w(f"== saving of (c) vs (a) and vs (b), whole-system ΣPss, t = {t:g}s ==")
        w("%-3s %8s %10s %10s %10s %10s" % ("N", "a MiB", "b MiB", "c MiB",
                                            "c vs a", "c vs a+b(b)"))
        for n in ns:
            a = runs.get(("a", n))
            b = runs.get(("b", n))
            c = runs.get(("c", n))
            if not a or not c:
                continue
            sa = sample_of(a, t)
            sc = sample_of(c, t)
            sb = sample_of(b, t) if b else None
            if not sa or not sc:
                continue
            pa, pc = whole(sa)["pss"], whole(sc)["pss"]
            pb = whole(sb)["pss"] if sb else None
            w("%-3d %8.1f %10s %10.1f %10.2f%% %10s"
              % (n, pa, fmt(pb), pc, (pa - pc) / pa * 100.0,
                 ("%.2f%%" % ((pb - pc) / pb * 100.0)) if pb else "—"))
        # (b) vs (a) for completeness
        bits = []
        for n in ns:
            da, db = runs.get(("a", n)), runs.get(("b", n))
            sa = sample_of(da, t) if da else None
            sb = sample_of(db, t) if db else None
            if sa and sb:
                pa, pb = whole(sa)["pss"], whole(sb)["pss"]
                bits.append("%d: %+.2f%%" % (n, (pa - pb) / pa * 100.0))
            else:
                bits.append("%d: —" % n)
        w("  (b) vs (a), same time point: " + ", ".join(bits))
        w("")

    # --------------------------------------------------------------- startup
    w("== startup and host CPU to bring N sessions to their first correct frame ==")
    w("%-3s %-3s %10s %12s %12s %10s %s"
      % ("arm", "N", "first ms", "per extra", "all ready", "CPU-s", "CPU-s no driver"))
    for arm in ARMS:
        for n in ns:
            d = runs.get((arm, n))
            if not d:
                continue
            w("%-3s %-3d %10s %12s %12s %10s %s"
              % (arm, n, fmt(d.get("first_session_ready_ms"), "%.0f"),
                 fmt(d.get("per_extra_session_ready_ms"), "%.0f"),
                 fmt(d.get("all_ready_ms"), "%.0f"),
                 fmt(d.get("cpu_bringup_seconds"), "%.2f"),
                 fmt(d.get("cpu_bringup_seconds_no_driver"), "%.2f")))
    w("")

    # ----------------------------------------------------------- correctness
    w("== correctness (same checks for every arm) ==")
    w("%-3s %-3s %6s %8s %10s %12s %14s %14s %s"
      % ("arm", "N", "sess", "verdict", "isolation", "md5 init eq", "md5 init",
         "md5 tap", "counts"))
    for arm in ARMS:
        for n in ns:
            d = runs.get((arm, n))
            if not d:
                continue
            ses = d.get("sessions") or {}
            iso = n * (n - 1) // 2
            counts0 = {s.get("count_initial") for s in ses.values()}
            counts1 = {s.get("count_after_tap") for s in ses.values()}
            md5i = {s.get("md5_initial") for s in ses.values()}
            md5t = {s.get("md5_after_tap") for s in ses.values()}
            w("%-3s %-3d %6d %8s %10d %12s %14s %14s %s"
              % (arm, n, len(ses), d.get("verdict"), iso,
                 d.get("md5_initial_all_equal"),
                 ",".join(sorted(x[:8] for x in md5i if x)) or "—",
                 ",".join(sorted(x[:8] for x in md5t if x)) or "—",
                 f"{sorted(counts0)}->{sorted(counts1)}"))
    w("")

    # ------------------------------------------------------------ swap proof
    w("== swap: per-process SwapPss and the scope's memory.swap.max ==")
    for arm in ARMS:
        for n in ns:
            d = runs.get((arm, n))
            if not d:
                continue
            s = sample_of(d, times[-1])
            if not s:
                continue
            cg = s.get("cgroups") or {}
            swmax = sorted({v.get("memory.swap.max") for v in cg.values()})
            cur = sorted({v.get("memory.swap.current") for v in cg.values()})
            w("(%s) N=%-3d scope swap.max=%s swap.current=%s  ΣSwapPss=%.3f MiB "
              "ΣSwap=%.3f MiB  oom.group=%s"
              % (arm, n, swmax, cur, whole(s)["swap_pss"] / MB,
                 sum(r.get("swap", 0) for r in s["processes"]) / MB,
                 sorted({v.get("memory.oom.group") for v in cg.values()})))
    w("")

    # ----------------------------------------------------------- release probe
    for (arm, n), d in sorted(runs.items()):
        p = d.get("release_probe")
        if not p:
            continue
        series = p["series"]
        w(f"== release probe: (arm {arm}, N={n}), 1 Hz, {p['seconds']:g}s, "
          f"{len(series)} samples ==")
        kids = []
        for s in series:
            for r in s["proc"]:
                if r.get("app_child"):
                    kids.append(r["pid"])
        kids = sorted(set(kids))
        w("app child pids: " + ", ".join(str(k) for k in kids))
        hdr = "%-6s" % "t(s)"
        for k in kids:
            hdr += " %16s" % f"pid {k}"
        hdr += " %10s" % "Σtree"
        w(hdr + "   (Pss/PD MiB)")
        for s in series:
            line = "%-6.1f" % s["t_s"]
            for k in kids:
                row = next((r for r in s["proc"] if r["pid"] == k), None)
                line += " %16s" % ("—" if not row else
                                   "%.1f/%.1f" % (row["rollup"]["Pss"] / MB,
                                                  row["rollup"]["Private_Dirty"] / MB))
            line += " %10.1f" % (s["sum"]["pss"] / MB)
            w(line)
        # buckets for the child with the biggest drop
        best = None
        for k in kids:
            rows = [(s["t_s"], next((r for r in s["proc"] if r["pid"] == k), None))
                    for s in series]
            rows = [(t, r) for t, r in rows if r and r.get("attr")]
            if len(rows) < 2:
                continue
            drop = rows[0][1]["rollup"]["Pss"] - rows[-1][1]["rollup"]["Pss"]
            if best is None or drop > best[1]:
                best = (k, drop, rows)
        if best:
            k, drop, rows = best
            w(f"child {k}: largest Pss drop {drop / MB:.1f} MiB over the window; "
              f"bucket attribution (Pss MiB):")
            for t, r in rows:
                b = r["attr"]["buckets"]
                w("  t=%-6.1f %s   tot=%.1f PD=%.1f" % (
                    t, "  ".join("%s=%.1f" % (name, b[name]["pss"] / MB)
                                 for name in ("region_space", "zygote_space", "image",
                                              "malloc", "dalvik_other", "other")),
                    r["attr"]["total"]["pss"] / MB,
                    r["attr"]["total"]["private_dirty"] / MB))
            w("  first/last largest mappings:")
            for row, tag in ((rows[0][1], "first"), (rows[-1][1], "last")):
                for m in row["attr"]["top"][:5]:
                    w("    %-5s %-46s pss=%8.1f pd=%8.1f (%s)"
                      % (tag, m["name"][:46], m["pss"] / MB,
                         m["private_dirty"] / MB, m["bucket"]))
        logs = [dict(l, t_s=s["t_s"]) for s in series for l in (s.get("logs") or [])]
        w(f"guest log lines matching GC/trim/Zygote during the probe: {len(logs)}")
        for l in logs[:24]:
            w("  t=%-6.1f [%s] %s" % (l["t_s"], l.get("session"), l["message"][:170]))
        w("")

    text = "\n".join(out) + "\n"
    print(text)
    slim = {"cells": [], "marginals": [], "probe_notes": []}
    for (arm, n), d in sorted(runs.items()):
        for t in times:
            s = sample_of(d, t)
            if not s:
                continue
            ch = children(s)
            slim["cells"].append({
                "arm": arm, "n": n, "t_s": t,
                "pss": round(whole(s)["pss"], 2),
                "pd": round(whole(s)["pd"], 2),
                "swap_pss": round(whole(s)["swap_pss"], 3),
                "launchers": round(whole(s, {"launcher"})["pss"], 2),
                "children": (None if not ch else {
                    "n": ch["n"], "pss_mean": round(ch["pss_mean"], 2),
                    "pd_mean": round(ch["pd_mean"], 2)}),
            })
    with open(os.path.join(HERE, "summary.json"), "w") as f:
        json.dump({"cells": slim["cells"],
                   "verdicts": {f"{a}-{n}": d.get("verdict")
                                for (a, n), d in sorted(runs.items())},
                   "knobs": {f"{a}-{n}": d.get("knobs")
                             for (a, n), d in sorted(runs.items())}},
                  f, indent=1, sort_keys=True)
    with open(os.path.join(HERE, "table.txt"), "w") as f:
        f.write(text)


if __name__ == "__main__":
    main()
