#!/usr/bin/env python3
"""GLSHIP A/B summary: default software GL ON vs `ACOS_GUEST_GLES=off`.

Reads results/handoff/GLSHIP/ab-<app>-<arm>-<n>/verdict.json (written by
run-ab.sh) and prints per-arm medians for the M8 fast action→state latency, the
total verify wall, the launch step and the session's peak RSS, plus the verdict
status of every rep.

Indicative only: these are shared-slot runs (other agents' ART work runs
concurrently), so the numbers are compared arm-to-arm, never to a published
baseline.
"""
import json
import os
import statistics as st
import sys

ROOT = os.path.dirname(os.path.abspath(__file__))
ARMS = ("on", "off")
APPS = ("counter", "form")
REPS = (1, 2, 3, 4, 5)
INPUT_STEPS = {"tap", "type", "swipe", "key", "scroll"}


def load(app, arm, n):
    path = os.path.join(ROOT, f"ab-{app}-{arm}-{n}", "verdict.json")
    if not os.path.isfile(path):
        return None
    with open(path) as f:
        return json.load(f)


def verify_ms(app, arm, n):
    """The provider verify cost this launch paid (`ACOS_GUEST_GLES_TRACE`)."""
    path = os.path.join(ROOT, f"ab-{app}-{arm}-{n}", "launcher.stderr.log")
    if not os.path.isfile(path):
        return None
    with open(path, errors="replace") as f:
        for line in f:
            if "verify " in line and "staged" in line:
                try:
                    return int(line.split("verify ")[1].split("ms")[0])
                except (IndexError, ValueError):
                    return None
    return None


def metrics(v):
    res = v.get("resources") or {}
    steps = v.get("steps") or []
    launch = steps[0]["duration_ms"] if steps else None
    # Only the steps that actually ran with the fast rule: a plain tap in the
    # same scenario (e.g. focusing a field, which does not change the tree) is
    # a strict settle and must not be averaged into the M8 fast latency.
    fast = [
        s["duration_ms"]
        for s in steps
        if s.get("step") in INPUT_STEPS
        and (s.get("settle") or {}).get("mode") == "fast"
    ]
    return {
        "status": v.get("status"),
        "wall_ms": res.get("wall_ms"),
        "peak_rss_mib": round(res.get("peak_rss_bytes", 0) / (1024 * 1024), 1),
        "launch_ms": launch,
        "m8_fast_ms": sum(fast) if fast else None,
        "m8_n": len(fast),
    }


def med(xs):
    xs = [x for x in xs if x is not None]
    return round(st.median(xs), 1) if xs else None


def main():
    rows = {}
    statuses = {}
    for app in APPS:
        for arm in ARMS:
            got = []
            for n in REPS:
                v = load(app, arm, n)
                if v is None:
                    got.append({"status": "MISSING"})
                    continue
                got.append(metrics(v))
            rows[(app, arm)] = got
            statuses[(app, arm)] = [g["status"] for g in got]

    out = []
    out.append("# GLSHIP cost A/B (indicative, shared slots)\n")
    out.append(
        "Default software GL (`angle-swiftshader`, no flag) vs "
        "`ACOS_GUEST_GLES=off`, 5 interleaved reps each, private runtime dir per "
        "rep. `m8_fast_ms` = sum of the tap/type step durations with "
        "`settle: fast` (the action→state latency); `peak_rss_mib` = the "
        "session's own `resources.peak_rss_bytes` high-water mark; `launch_ms` = "
        "the launch step (the only step that pays the provider bind/props).\n"
    )
    for app in APPS:
        out.append(f"## {app}\n")
        out.append(
            "| arm | verdicts | wall p50 | launch p50 | M8 fast p50 | peak RSS p50 |"
        )
        out.append("|---|---|---|---|---|---|")
        for arm in ARMS:
            g = rows[(app, arm)]
            label = "default ON" if arm == "on" else "off"
            out.append(
                "| {} | {} | {} ms | {} ms | {} ms | {} MiB |".format(
                    label,
                    ",".join(str(s) for s in statuses[(app, arm)]),
                    med([x.get("wall_ms") for x in g]),
                    med([x.get("launch_ms") for x in g]),
                    med([x.get("m8_fast_ms") for x in g]),
                    med([x.get("peak_rss_mib") for x in g]),
                )
            )
            if arm == "on":
                out.append(
                    "\n* provider verify cost per launch (ms, rep 1..5): "
                    f"{[verify_ms(app, arm, n) for n in REPS]}"
                )
        on, off = rows[(app, "on")], rows[(app, "off")]
        for field, unit in (
            ("wall_ms", "ms"),
            ("launch_ms", "ms"),
            ("m8_fast_ms", "ms"),
            ("peak_rss_mib", "MiB"),
        ):
            a, b = med([x.get(field) for x in on]), med([x.get(field) for x in off])
            if a is None or b is None:
                continue
            delta = a - b
            pct = (100.0 * delta / b) if b else 0.0
            out.append(
                f"\n* `{field}`: on {a} {unit} vs off {b} {unit} "
                f"(Δ {delta:+.1f} {unit}, {pct:+.1f} %)"
            )
        out.append("")
    text = "\n".join(out) + "\n"
    sys.stdout.write(text)


if __name__ == "__main__":
    main()
