#!/usr/bin/env python3
"""Compare the static native reachability estimates (research/framework-dex)
with the measured dynamic native work-list (this task).

Static format: lines "NATIVE\\tL<class>;\\t<name>\\t<proto>".
Measured format:  the natives_*.tsv written by art_trace.py.

CLI:
    compare_static_dynamic_natives.py <static.txt> <natives.tsv> <label> [--out TSV]
"""
import argparse
import sys
from collections import Counter


def load_static(path):
    out = set()
    for line in open(path, errors="replace"):
        if not line.startswith("NATIVE\t"):
            continue
        p = line.rstrip("\n").split("\t")
        if len(p) < 4:
            continue
        desc, name, proto = p[1], p[2], p[3]
        cls = desc[1:-1].replace("/", ".") if desc.startswith("L") else desc
        out.add((cls, name, proto))
    return out


def load_measured(path):
    out = {}
    for line in open(path, errors="replace"):
        p = line.rstrip("\n").split("\t")
        if len(p) < 7 or p[0] == "enters":
            continue
        enters, _, first, where, cls, name, sig = p[:7]
        out[(cls, name, sig)] = (int(enters), where)
    return out


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("static")
    ap.add_argument("measured")
    ap.add_argument("label")
    ap.add_argument("--out")
    args = ap.parse_args()

    static = load_static(args.static)
    measured = load_measured(args.measured)

    both = sorted(static & set(measured))
    pred_only = sorted(static - set(measured))
    meas_only = sorted(set(measured) - static)

    if args.out:
        with open(args.out, "w") as f:
            f.write("class\tmethod\tsignature\tpredicted\tmeasured\tenters\twhere\n")
            for key in sorted(static | set(measured)):
                cls, name, sig = key
                pred = "yes" if key in static else "no"
                m = measured.get(key)
                f.write("%s\t%s\t%s\t%s\t%s\t%s\t%s\n" % (
                    cls, name, sig, pred, "yes" if m else "no",
                    m[0] if m else "", m[1] if m else ""))

    def by_pkg(keys):
        c = Counter()
        for cls, _, _ in keys:
            parts = cls.split(".")
            if cls.startswith("android.graphics"):
                c["android.graphics"] += 1
            elif cls.startswith("android.view"):
                c["android.view"] += 1
            elif cls.startswith("android.os"):
                c["android.os"] += 1
            elif cls.startswith("android.content.res"):
                c["android.content.res"] += 1
            elif cls.startswith("android.text") or cls.startswith("android.graphics.text"):
                c["android.text"] += 1
            elif cls.startswith("android.util"):
                c["android.util"] += 1
            elif cls.startswith("android."):
                c["android.* (other)"] += 1
            elif cls.startswith(("java.", "javax.", "jdk.", "sun.", "dalvik.", "libcore.")):
                c["core " + ".".join(parts[:2])] += 1
            else:
                c["other"] += 1
        return c

    print("== static vs dynamic natives: %s ==" % args.label)
    print("static predicted: %d" % len(static))
    print("measured used:    %d" % len(measured))
    print("both (predicted AND used):        %d" % len(both))
    print("predicted-but-unused:             %d" % len(pred_only))
    print("used-but-NOT-predicted:           %d" % len(meas_only))
    print("measured native classes: %d" % len({c for c, _, _ in measured}))
    print()
    print("measured used by package:")
    for k, v in by_pkg(set(measured)).most_common():
        print("  %-28s %d" % (k, v))
    print()
    print("predicted-but-unused by package (top):")
    for k, v in by_pkg(set(pred_only)).most_common(12):
        print("  %-28s %d" % (k, v))
    print()
    print("used-but-not-predicted:")
    for cls, name, sig in meas_only:
        print("  %s.%s%s  x%d" % (cls, name, sig, measured[(cls, name, sig)][0]))


if __name__ == "__main__":
    main()
