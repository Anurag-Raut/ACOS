#!/usr/bin/env python3
"""Extract (class, method, proto) -> insns size from a DEX via dexdump -d.

Streaming; writes TSV to stdout. Used to estimate the interpreted-instruction
workload of the measured dynamic method trace (research/service-layer/REPORT.md
section 8). Not part of the runtime.
"""
import subprocess, sys, os
DEXDUMP = os.path.expanduser("~/Android/Sdk/build-tools/34.0.0/dexdump")

def scan(path):
    p = subprocess.Popen([DEXDUMP, "-d", path], stdout=subprocess.PIPE,
                         stderr=subprocess.DEVNULL)
    cur = None; meth = None; proto = None
    for b in p.stdout:
        s = b.decode("utf-8", "replace").strip()
        if s.startswith("Class descriptor"):
            cur = s.split(":", 1)[1].strip().strip("'")
            meth = None; proto = None
        elif s.startswith("name") and "'" in s:
            meth = s.split("'")[1]
        elif s.startswith("type") and "'" in s:
            proto = s.split("'")[1]
        elif s.startswith("insns size"):
            try:
                n = int(s.split(":")[1].split()[0])
            except Exception:
                continue
            if cur and meth:
                print("\t".join([cur, meth, proto or "", str(n)]))
    p.wait()

for path in sys.argv[1:]:
    scan(path)
