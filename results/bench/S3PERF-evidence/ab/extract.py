#!/usr/bin/env python3
"""S3PERF A/B extractor: interleaved main-vs-fix latency reps.

Reads results/bench/S3PERF-evidence/ab/{main,fix}-r{1,2,3}.json (acos-bench.sh
merged docs, one `--reps 1 --groups "a:latency:8G:300"` acquisition per file)
and prints per-metric per-rep p50, the 3-rep median per arm, the delta and the
+/-5 % verdict.
"""
import json
import statistics
from pathlib import Path

HERE = Path(__file__).resolve().parent
METRICS = [
    ("m4_launch_strict_ms", "M4 warm launch strict"),
    ("m4_launch_fast_ms", "M4 warm launch fast"),
    ("m6_reset_strict_ms", "M6 reset strict"),
    ("m6_reset_fast_ms", "M6 reset fast"),
    ("m7_tree_strict_ms", "M7 tree strict"),
    ("m7_tree_fast_ms", "M7 tree fast"),
    ("m8_tap_counter_strict_ms", "M8 tap strict"),
    ("m8_tap_counter_fast_ms", "M8 tap fast"),
    ("m5_screenshot_ms", "M5 screenshot"),
    ("tap_ack_ms", "tap ack"),
]


def arm(name):
    rows, shas, ns = {}, set(), {}
    for r in (1, 2, 3):
        p = HERE / f"{name}-r{r}.json"
        if not p.is_file():
            continue
        d = json.loads(p.read_text())
        shas.add(d.get("binary_sha256"))
        for key, _ in METRICS:
            m = d["metrics"].get(key)
            if m and m.get("ok"):
                rows.setdefault(key, []).append(m["p50"])
                ns.setdefault(key, []).append(m.get("n"))
        print(f"# {p.name}: sha256={d.get('binary_sha256')} reps={d.get('reps')}")
    return rows, shas, ns


def main():
    main_rows, main_sha, main_n = arm("main")
    fix_rows, fix_sha, fix_n = arm("fix")
    print("\narm shas:", {"main": sorted(main_sha), "fix": sorted(fix_sha)})
    print("\n| metric | main p50 med (reps) | fix p50 med (reps) | delta | verdict (+/-5%) |")
    print("|---|---:|---:|---:|---|")
    doc = {"main_sha256": sorted(main_sha), "fix_sha256": sorted(fix_sha),
           "metrics": {}}
    for key, label in METRICS:
        a = main_rows.get(key, [])
        b = fix_rows.get(key, [])
        if not a or not b:
            continue
        am, bm = statistics.median(a), statistics.median(b)
        delta = (bm - am) / am * 100.0 if am else 0.0
        verdict = "pass" if abs(delta) <= 5.0 else "FAIL"
        print(f"| {label} | {am:.3f} ({', '.join(f'{x:.1f}' for x in a)}) | "
              f"{bm:.3f} ({', '.join(f'{x:.1f}' for x in b)}) | "
              f"{delta:+.1f} % | {verdict} |")
        doc["metrics"][key] = {"label": label, "main_p50_reps": a,
                               "fix_p50_reps": b, "main_p50_median": am,
                               "fix_p50_median": bm, "delta_pct": delta,
                               "verdict": verdict,
                               "main_n": main_n.get(key),
                               "fix_n": fix_n.get(key)}
    (HERE / "ab-summary.json").write_text(json.dumps(doc, indent=1) + "\n")


if __name__ == "__main__":
    main()
