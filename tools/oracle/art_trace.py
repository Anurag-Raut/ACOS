#!/usr/bin/env python3
"""Parse an ART method-trace file (the `-Xmethod-trace` / `am profile` format).

ART trace format (version 3, observed on API 34):

    *version
    3
    <key=value metadata>...
    *threads
    <tid>\\t<thread name>
    *methods
    <method id hex>\\t<class>\\t<method>\\t<signature>\\t<file>
    *end
    [SLOW]                       # 4-byte marker when clock=dual
    <14-byte records...>

Each record is little-endian:
    u16 thread_id
    u32 method_id_and_flags     # method_id = value & ~3; bit0 = 1 means EXIT
    u32 thread_cpu_usec
    u32 wall_clock_usec

This parser determines the record alignment automatically (there may be a
14-byte preamble before the first real record).

CLI:
    art_trace.py parse <trace> [--native-set FILE] [--main-class NAME]
                       [--json OUT] [--data-dir DIR] [--label NAME]
"""
import argparse
import json
import os
import re
import struct
import sys
from collections import Counter, defaultdict


def parse_header(data):
    if not data.startswith(b"*version"):
        raise ValueError("not an ART trace (missing *version)")
    threads = {}
    methods = {}
    ti = data.find(b"*threads\n")
    mi = data.find(b"*methods\n")
    end = data.find(b"*end\n")
    if ti < 0 or mi < 0 or end < 0:
        raise ValueError("malformed ART trace header")
    ti += len(b"*threads\n")
    mi += len(b"*methods\n")
    for line in data[ti:mi - len(b"*methods\n")].split(b"\n"):
        if b"\t" in line and line.strip():
            t, n = line.split(b"\t", 1)
            try:
                threads[int(t)] = n.decode("utf-8", "replace")
            except ValueError:
                pass
    for line in data[mi:end].split(b"\n"):
        if not line.strip():
            continue
        p = line.split(b"\t")
        if len(p) >= 4:
            try:
                mid = int(p[0], 16)
            except ValueError:
                continue
            methods[mid] = (p[1].decode("utf-8", "replace"),
                            p[2].decode("utf-8", "replace"),
                            p[3].decode("utf-8", "replace"))
    return threads, methods, end + len(b"*end\n")


def parse_records(data, start, methods, threads):
    """Return (records, chosen_offset). records = list of dicts."""
    # Skip the "SLOW" marker (clock=dual writes it) if present.
    blob = data[start:]
    if blob[:4] == b"SLOW":
        blob = blob[4:]

    def decode(off):
        recs = []
        p = off
        while p + 14 <= len(blob):
            tid = struct.unpack_from("<H", blob, p)[0]
            raw = struct.unpack_from("<I", blob, p + 2)[0]
            cpu = struct.unpack_from("<I", blob, p + 6)[0]
            wall = struct.unpack_from("<I", blob, p + 10)[0]
            mid = raw & ~3
            recs.append({"tid": tid, "mid": mid, "exit": bool(raw & 1),
                         "cpu": cpu, "wall": wall})
            p += 14
        return recs

    best = None
    for off in range(0, 14):
        recs = decode(off)
        if not recs:
            continue
        valid = sum(1 for r in recs if r["mid"] in methods and r["tid"] in threads)
        score = valid / len(recs)
        if best is None or score > best[0] or (score == best[0] and len(recs) > len(best[2])):
            best = (score, off, recs)
    if best is None:
        return [], 0
    return best[2], best[1]


def load_native_set(path):
    natives = set()
    if not path:
        return natives
    with open(path) as f:
        for line in f:
            p = line.rstrip("\n").split("\t")
            if len(p) >= 3:
                natives.add((p[0], p[1], p[2]))
    return natives


def analyse(trace_path, native_path=None, main_class="Main", label=None):
    data = open(trace_path, "rb").read()
    threads, methods, start = parse_header(data)
    recs, off = parse_records(data, start, methods, threads)
    natives_set = load_native_set(native_path)

    main_ids = {mid for mid, (c, m, s) in methods.items() if c == main_class and m == "main"}

    enters = Counter()
    exits = Counter()
    first_order = {}
    first_wall = {}
    order = 0
    seen_main = False
    before_main_natives = Counter()
    order_before_main = {}
    clinit_first = {}
    clinit_wall = {}
    per_thread = defaultdict(int)

    # stack-based validation per thread
    stacks = defaultdict(list)
    balance_ok = balance_bad = 0

    for r in recs:
        per_thread[r["tid"]] += 1
        mid = r["mid"]
        if mid not in methods:
            continue
        cls, meth, sig = methods[mid]
        if r["exit"]:
            exits[mid] += 1
            if stacks[r["tid"]] and stacks[r["tid"]][-1] == mid:
                stacks[r["tid"]].pop()
                balance_ok += 1
            else:
                balance_bad += 1
            continue
        # enter
        enters[mid] += 1
        if mid not in first_order:
            first_order[mid] = order
            first_wall[mid] = r["wall"]
            order += 1
        stacks[r["tid"]].append(mid)
        if meth == "<clinit>" and cls not in clinit_first:
            clinit_first[cls] = first_order[mid]
            clinit_wall[cls] = r["wall"]
        if not seen_main and mid in main_ids:
            seen_main = True
        if not seen_main and (cls, meth, sig) in natives_set:
            if (cls, meth, sig) not in order_before_main:
                order_before_main[(cls, meth, sig)] = order
            before_main_natives[(cls, meth, sig)] += 1

    # Build method rows (sorted by total enter count desc).
    method_rows = []
    for mid, cnt in enters.items():
        cls, meth, sig = methods[mid]
        method_rows.append({
            "class": cls, "method": meth, "signature": sig,
            "enters": cnt, "exits": exits[mid],
            "first_order": first_order.get(mid),
            "first_wall_usec": first_wall.get(mid),
            "native": (cls, meth, sig) in natives_set,
        })
    method_rows.sort(key=lambda r: (-r["enters"], r["first_order"] if r["first_order"] is not None else 1 << 30))

    natives = [r for r in method_rows if r["native"]]
    native_classes = Counter()
    for r in natives:
        native_classes[r["class"]] += 1

    clinit_order = sorted(clinit_first.items(), key=lambda kv: kv[1])
    classes_executed = sorted({methods[mid][0] for mid in enters})

    def category(cls):
        if cls.startswith(("java.", "javax.", "jdk.", "sun.", "dalvik.", "libcore.",
                            "com.android.icu.", "org.apache.", "org.json.", "org.w3c.",
                            "org.xml.")):
            return "core"
        if cls.startswith(("android.", "com.android.internal.")):
            return "android"
        return "other"

    def package_of(cls):
        parts = cls.split(".")
        if len(parts) >= 3 and parts[0] == "com" and parts[1] == "android":
            return "com.android." + parts[2] if len(parts) > 3 else "com.android"
        return ".".join(parts[:2]) if len(parts) >= 2 else cls

    pkg_totals = {}
    cat_totals = {"core": {"methods": 0, "enters": 0},
                  "android": {"methods": 0, "enters": 0},
                  "other": {"methods": 0, "enters": 0}}
    for r in method_rows:
        p = package_of(r["class"])
        e = pkg_totals.setdefault(p, {"methods": 0, "enters": 0})
        e["methods"] += 1
        e["enters"] += r["enters"]
        c = category(r["class"])
        cat_totals[c]["methods"] += 1
        cat_totals[c]["enters"] += r["enters"]
    native_cat = {"core": 0, "android": 0, "other": 0}
    for r in natives:
        native_cat[category(r["class"])] += 1

    result = {
        "trace": trace_path,
        "label": label,
        "record_offset": off,
        "header": {
            "num_threads": len(threads),
            "num_methods": len(methods),
            "record_count": len(recs),
            "enter_events": sum(enters.values()),
            "exit_events": sum(exits.values()),
            "balance_ok": balance_ok,
            "balance_bad": balance_bad,
        },
        "threads": dict(sorted(per_thread.items(), key=lambda kv: -kv[1])),
        "distinct_methods_executed": len(enters),
        "total_method_enters": sum(enters.values()),
        "methods": method_rows,
        "natives": natives,
        "native_class_count": len(native_classes),
        "native_classes": dict(sorted(native_classes.items(), key=lambda kv: -kv[1])),
        "natives_before_main": [
            {"class": c, "method": m, "signature": s, "enters": before_main_natives[(c, m, s)],
             "first_order": order_before_main[(c, m, s)]}
            for (c, m, s) in sorted(order_before_main, key=lambda k: order_before_main[k])
        ],
        "category_totals": cat_totals,
        "native_category_totals": native_cat,
        "package_totals": dict(sorted(pkg_totals.items(), key=lambda kv: -kv[1]["enters"])),
        "classes_executed": classes_executed,
        "classes_initialised": [
            {"class": c, "first_order": o, "first_wall_usec": clinit_wall[c]}
            for c, o in clinit_order
        ],
    }
    return result, threads, methods


def write_outputs(res, data_dir, label, main_class):
    os.makedirs(data_dir, exist_ok=True)
    with open(os.path.join(data_dir, f"methods_{label}.tsv"), "w") as f:
        f.write("enters\texits\tfirst_order\tclass\tmethod\tsignature\tnative\n")
        for r in res["methods"]:
            f.write("%d\t%d\t%s\t%s\t%s\t%s\t%s\n" % (
                r["enters"], r["exits"], r["first_order"], r["class"],
                r["method"], r["signature"], "native" if r["native"] else "bytecode"))
    with open(os.path.join(data_dir, f"natives_{label}.tsv"), "w") as f:
        f.write("enters\texits\tfirst_order\tcall_site\tclass\tmethod\tsignature\n")
        for r in res["natives"]:
            where = "before_main" if r["first_order"] is not None and any(
                b["class"] == r["class"] and b["method"] == r["method"] and b["signature"] == r["signature"]
                for b in res["natives_before_main"]) else "program"
            f.write("%d\t%d\t%s\t%s\t%s\t%s\t%s\n" % (
                r["enters"], r["exits"], r["first_order"], where, r["class"],
                r["method"], r["signature"]))
    with open(os.path.join(data_dir, f"classes_initialised_{label}.txt"), "w") as f:
        for r in res["classes_initialised"]:
            f.write("%s\n" % r["class"])


def main():
    ap = argparse.ArgumentParser()
    sub = ap.add_subparsers(dest="cmd", required=True)
    p = sub.add_parser("parse")
    p.add_argument("trace")
    p.add_argument("--native-set")
    p.add_argument("--main-class", default="Main")
    p.add_argument("--json")
    p.add_argument("--data-dir")
    p.add_argument("--label")
    p.add_argument("--top", type=int, default=15)
    args = ap.parse_args()

    label = args.label or os.path.splitext(os.path.basename(args.trace))[0]
    res, threads, methods = analyse(args.trace, args.native_set, args.main_class, label)
    if args.data_dir:
        write_outputs(res, args.data_dir, label, args.main_class)
    if args.json:
        with open(args.json, "w") as f:
            json.dump(res, f, indent=2)
            f.write("\n")
    print("== %s ==" % label)
    h = res["header"]
    print("threads=%d methods=%d records=%d enters=%d exits=%d balance_ok=%d balance_bad=%d" % (
        h["num_threads"], h["num_methods"], h["record_count"], h["enter_events"],
        h["exit_events"], h["balance_ok"], h["balance_bad"]))
    print("distinct methods executed: %d" % res["distinct_methods_executed"])
    print("distinct native methods invoked: %d (in %d classes)" % (
        len(res["natives"]), res["native_class_count"]))
    print("classes initialised (<clinit>): %d" % len(res["classes_initialised"]))
    print("top methods:")
    for r in res["methods"][:args.top]:
        print("  %6d  %s.%s%s%s" % (r["enters"], r["class"], r["method"], r["signature"],
                                    "  [native]" if r["native"] else ""))


if __name__ == "__main__":
    main()
