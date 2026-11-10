#!/usr/bin/env python3
"""Canonicalize `dexdump -d` output into one line per method.

Each output line has the form:

    M <class-descriptor> <method-name> <proto> <count> <mnemonic>...

which is directly comparable with `dexdump --example ... --mnemonics`.

`dexdump` prints string literals with their bytes unescaped, so a method with a
string containing a newline/CR splits its `const-string` line across several
physical lines. While inside a method body we therefore ignore any line that is
not an instruction or a known section terminator.
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
    # Lines that end a method body or begin the next member.
    re_terminator = re.compile(
        r"^\s+(catches|positions|locals)\b|^\s+name\s*:|^Class descriptor|"
        r"^\s+insns size|^\s+#\d+\s*:"
    )

    for ln in lines:
        m = re_class.search(ln)
        if m:
            flush()
            cls = m.group(1)
            continue
        m = re_name.match(ln)
        if m:
            flush()
            name = m.group(1)
            continue
        if not in_code:
            m = re_type.match(ln)
            if m:
                typ = m.group(1)
                continue
        if "insns size" in ln:
            flush()
            in_code = True
            seq = []
            continue
        if in_code:
            m = re_insn.search(ln)
            if m:
                seq.append(m.group(2))
            elif re_terminator.search(ln):
                flush()
            # else: continuation of an instruction line (embedded newline
            # inside a string literal); ignore.
            continue

    flush()
    return 0


if __name__ == "__main__":
    sys.exit(main())
