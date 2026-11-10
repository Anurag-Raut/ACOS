#!/usr/bin/env python3
"""Extract a launch timeline from an ART method trace.

Finds the first occurrence (wall-clock usec since trace start) of a set of
well-known launch milestones and writes JSON.

CLI: timeline_from_trace.py <trace> [--json OUT] [--app-pkg PKG]
"""
import argparse
import json
import sys

sys.path.insert(0, __import__("os").path.dirname(__file__))
from art_trace import parse_header, parse_records  # noqa: E402


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("trace")
    ap.add_argument("--json")
    ap.add_argument("--app-pkg", default="")
    args = ap.parse_args()

    data = open(args.trace, "rb").read()
    threads, methods, start = parse_header(data)
    recs, _ = parse_records(data, start, methods, threads)

    # (label, class, method)
    markers = [
        ("zygote_fork_or_main", "com.android.internal.os.ZygoteInit", "main"),
        ("activity_thread_main", "android.app.ActivityThread", "main"),
        ("handle_bind_application", "android.app.ActivityThread", "handleBindApplication"),
        ("perform_launch_activity", "android.app.ActivityThread", "performLaunchActivity"),
        ("application_onCreate", "android.app.Application", "onCreate"),
        ("activity_performCreate", "android.app.Activity", "performCreate"),
        ("activity_onCreate_app", args.app_pkg + ".MainActivity", "onCreate"),
        ("activity_performResume", "android.app.Activity", "performResume"),
        ("activity_onResume_app", args.app_pkg + ".MainActivity", "onResume"),
        ("viewroot_setView", "android.view.ViewRootImpl", "setView"),
        ("performTraversals", "android.view.ViewRootImpl", "performTraversals"),
        ("performMeasure", "android.view.ViewRootImpl", "performMeasure"),
        ("performLayout", "android.view.ViewRootImpl", "performLayout"),
        ("performDraw", "android.view.ViewRootImpl", "performDraw"),
        ("choreographer_doFrame", "android.view.Choreographer", "doFrame"),
        ("hardware_renderer_draw", "android.graphics.HardwareRenderer", "draw"),
        ("threaded_renderer_draw", "android.view.ThreadedRenderer", "draw"),
        ("surface_control_ctor", "android.view.SurfaceControl", "<init>"),
        ("activity_makeVisible", "android.app.ActivityThread", "handleResumeActivity"),
    ]

    found = {}
    last_wall = 0
    for r in recs:
        if r["mid"] in methods:
            _, _, _ = methods[r["mid"]]
        if r["wall"] > last_wall:
            last_wall = r["wall"]

    # Walk enters in order; record first wall for each marker.
    for r in recs:
        if r["exit"]:
            continue
        mid = r["mid"]
        if mid not in methods:
            continue
        cls, meth, _ = methods[mid]
        for label, mc, mm in markers:
            if label in found:
                continue
            if cls == mc and meth == mm:
                found[label] = {"wall_usec": r["wall"], "thread": r["tid"],
                                "class": cls, "method": meth}

    # header elapsed-time-usec
    header_elapsed = None
    mi = data.find(b"*methods\n")
    for line in data[:mi].split(b"\n"):
        if line.startswith(b"elapsed-time-usec="):
            header_elapsed = int(line.split(b"=", 1)[1])

    doc = {
        "trace": args.trace,
        "elapsed_time_usec": header_elapsed,
        "last_record_wall_usec": last_wall,
        "milestones": found,
    }
    if args.json:
        with open(args.json, "w") as f:
            json.dump(doc, f, indent=2)
            f.write("\n")
    print("== timeline %s ==" % args.trace)
    print("elapsed=%s usec  last_record=%d usec" % (header_elapsed, last_wall))
    for label, _, _ in markers:
        if label in found:
            print("  %-24s %8d us  tid=%d" % (label, found[label]["wall_usec"], found[label]["thread"]))
        else:
            print("  %-24s (not observed)" % label)


if __name__ == "__main__":
    main()
