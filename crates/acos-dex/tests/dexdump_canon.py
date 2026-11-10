#!/usr/bin/env python3
"""Canonicalize `dexdump -d` output into one line per method.

Each output line has the form:

    M <class-descriptor> <method-name> <proto> <count> <mnemonic>...

which is directly comparable with `dexdump --example ... --mnemonics`.
"""
import re
import sys


def main() -> int:
    path = sys.argv[1]
    with open(path, "r", errors="replace") as fh:
        lines = fh.read().splitlines()

    cls = ""
    name = ""
    typ = ""
    in_code = False
    seq: list[str] = []

    def flush() -> None:
        nonlocal in_code, seq
        if in_code:
            print(f"M {cls} {name} {typ} {len(seq)} {' '.join(seq)}")
        in_code = False
        seq = []

    re_class = re.compile(r"Class descriptor\s*:\s*'(.*)'")
    re_name = re.compile(r"^\s+name\s*:\s*'(.*)'")
    re_type = re.compile(r"^\s+type\s*:\s*'(.*)'")
    re_insn = re.compile(r"\|([0-9a-f]{4,}):\s+([A-Za-z0-9/_-]+)")

    for ln in lines:
        m = re_class.search(ln)
        if m:
            flush()
            cls = m.group(1)
            continue
        m = re_name.match(ln)
        if m:
            name = m.group(1)
            continue
        m = re_type.match(ln)
        if m:
            typ = m.group(1)
            continue
        if "insns size" in ln:
            flush()
            in_code = True
            seq = []
            continue
        m = re_insn.search(ln)
        if m and in_code:
            seq.append(m.group(2))
            continue
        # dexdump prints a method-start marker like `0016cc: |[0016cc] Foo.bar:()V`
        # right after the `insns size` line; it is not an instruction.
        if "|[" in ln:
            continue
        if in_code:
            flush()

    flush()
    return 0


if __name__ == "__main__":
    sys.exit(main())
