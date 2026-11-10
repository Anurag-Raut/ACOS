#!/usr/bin/env python3
"""Parse Android SDK `dexdump` output into a native-method set.

Usage:
    dexdump [-d] <classes.dex> | dex_native_methods.py

Emits TSV lines:  <class>\\t<method>\\t<signature>
for every method whose access flags include NATIVE.  `class` is in dotted form
(e.g. java.lang.System); `signature` is the DEX/JNI descriptor (e.g. (I)C).
"""
import re
import sys

class_re = re.compile(r"^Class descriptor  : '([^']*)'")
in_re = re.compile(r"#\d+\s+: \(in (L[^;]+;)\)")
name_re = re.compile(r"^\s*name\s*:\s*'([^']*)'")
type_re = re.compile(r"^\s*type\s*:\s*'([^']*)'")
access_re = re.compile(r"^\s*access\s*:\s*0x[0-9a-fA-F]+\s*\((.*)\)")


def dotted(desc):
    return desc[1:-1].replace("/", ".") if desc.startswith("L") else desc


def main():
    cur_class = None
    name = None
    sig = None
    for raw in sys.stdin.buffer:
        line = raw.decode("utf-8", "replace")
        m = class_re.match(line)
        if m:
            cur_class = dotted(m.group(1))
            continue
        m = in_re.search(line)
        if m:
            cur_class = dotted(m.group(1))
            continue
        m = name_re.match(line)
        if m:
            name = m.group(1)
            continue
        m = type_re.match(line)
        if m:
            sig = m.group(1)
            continue
        m = access_re.match(line)
        if m:
            if "NATIVE" in m.group(1) and cur_class and name is not None and sig is not None:
                sys.stdout.write(f"{cur_class}\t{name}\t{sig}\n")
            name = sig = None
            continue


if __name__ == "__main__":
    main()
