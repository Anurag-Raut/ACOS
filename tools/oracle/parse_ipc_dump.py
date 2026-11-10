#!/usr/bin/env python3
"""Parse an `am trace-ipc stop --dump-file` dump for one process.

The dump is a sequence of:

    Traces for process: <name>
    Count: <n>
    Trace: java.lang.Throwable
        at android.os.BinderProxy.transact(BinderProxy.java:553)
        at <interface>$Stub$Proxy.<method>(...)
        at ...
    ...

Because the dump is per-process, the section for the app gives outgoing
transactions (with interface + method + call count).  The section for `system`
contains incoming callbacks into the app.
"""
import argparse
import re
import sys
from collections import Counter

proc_re = re.compile(r"^Traces for process: (\S+)")
count_re = re.compile(r"^Count: (\d+)")
proxy_re = re.compile(r"at ([A-Za-z0-9_.$]+)\$Stub\$Proxy\.([A-Za-z0-9_$<>]+)\(")
stub_re = re.compile(r"at ([A-Za-z0-9_.$]+)\$Stub\.onTransact\(")


def parse(path, process):
    sections = {}
    cur = None
    pending = None
    with open(path, encoding="utf-8", errors="replace") as f:
        for line in f:
            m = proc_re.match(line)
            if m:
                cur = m.group(1)
                sections.setdefault(cur, Counter())
                pending = None
                continue
            m = count_re.match(line)
            if m:
                pending = int(m.group(1))
                continue
            m = proxy_re.search(line)
            if m and cur is not None:
                sections[cur][(m.group(1), m.group(2))] += pending or 1
                pending = None
                continue
    return sections


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("dump")
    ap.add_argument("--process", required=True)
    ap.add_argument("--out")
    ap.add_argument("--match", default="")
    args = ap.parse_args()
    sections = parse(args.dump, args.process)
    rows = []
    for proc, counter in sections.items():
        for (iface, meth), cnt in counter.items():
            if args.match and args.match not in proc:
                continue
            rows.append((proc, iface, meth, cnt))
    rows.sort(key=lambda r: (r[1], r[2]))
    if args.out:
        with open(args.out, "w") as f:
            f.write("process\tinterface\tmethod\tcount\n")
            for r in rows:
                f.write("%s\t%s\t%s\t%d\n" % r)
    for r in rows[:60]:
        print("%s  %s.%s  x%d" % (r[0], r[1], r[2], r[3]))
    print("total rows: %d" % len(rows))


if __name__ == "__main__":
    main()
