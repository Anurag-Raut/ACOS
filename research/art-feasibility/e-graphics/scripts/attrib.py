#!/usr/bin/env python3
"""Attribute JNINativeMethod tables (found by jninatives.py) to Java classes.

Every table is registered by a function that does, in essence:
    env->FindClass("android/graphics/RenderNode")
    env->RegisterNatives(clazz, gMethods, NELEM(gMethods))
so the table's address appears as a RIP-relative `lea` immediate inside that
function, and the class name literal appears as another `lea`-able .rodata
string in the same function.  We recover both by scanning .text for
RIP-relative leas whose target is either a table address or a .rodata string.
"""
import sys, json, struct, re
from jninatives import Elf, scan

LEA_OPCODES = (b'\x48\x8d\x05', b'\x48\x8d\x0d', b'\x48\x8d\x15', b'\x48\x8d\x1d',
               b'\x48\x8d\x25', b'\x48\x8d\x2d', b'\x48\x8d\x35', b'\x48\x8d\x3d')

def leas(e, lo, hi):
    """yield (file_off_of_insn, target_vaddr) for RIP-relative lea in [lo,hi)."""
    b = e.b
    i = lo
    while i < hi - 7:
        if b[i] == 0x48 and b[i + 1] == 0x8d and (b[i + 2] & 0xc7) == 0x05:
            disp = struct.unpack_from('<i', b, i + 3)[0]
            insn_end = i + 7
            # convert file off -> vaddr
            s, off = e.vaddr_to_file(i)
            target = s['addr'] + off + 7 + disp
            yield i, target
            i += 3
        else:
            i += 1

def attach(e):
    txt = e.by_name('.text')
    return list(leas(e, txt['off'], txt['off'] + txt['size']))

def main():
    paths = sys.argv[1:]
    for path in paths:
        e, ptr, tables = scan(path)
        leas = attach(e)
        by_target = {}
        for off, tgt in leas:
            by_target.setdefault(tgt, []).append(off)
        tab_addrs = {t['addr']: t for t in tables}
        print(f"== {path.split('/')[-1]}: {len(tables)} tables, {len(leas)} leas")
        for t in tables:
            refs = by_target.get(t['addr'], [])
            def _va(x):
                sec, o = e.vaddr_to_file(x)
                return None if sec is None else sec['addr'] + o
            t['refs'] = [hex(v) for v in (_va(x) for x in refs) if v is not None]
            # class name guess: nearest rodata string lea within +-0x400 of the ref
            guess = None
            for r in refs:
                s, off = e.vaddr_to_file(r)
                rva = s['addr'] + off
                cands = []
                for off2, tgt in leas:
                    s2, o2 = e.vaddr_to_file(off2)
                    va2 = s2['addr'] + o2
                    if abs(va2 - rva) > 0x400:
                        continue
                    sec, so = e.vaddr_to(tgt)
                    if sec is None or sec['name'] != '.rodata':
                        continue
                    st = e.cstr(tgt)
                    if st and re.fullmatch(r'[A-Za-z0-9_$./;]+', st) and ('/' in st or ';' in st or st[0].isupper()):
                        cands.append((abs(va2 - rva), st))
                if cands:
                    cands.sort()
                    guess = cands[0][1]
            t['class_guess'] = guess
            print(f"  @{t['addr']:#x} n={len(t['entries']):3d} class={guess}  "
                  f"first={t['entries'][0]['name']}")
        json.dump(tables, open(f"/home/Qui/Projects/acos/.cache/art-exp-e/tables-{path.split('/')[-1]}.json", 'w'), indent=1)

if __name__ == '__main__':
    main()
