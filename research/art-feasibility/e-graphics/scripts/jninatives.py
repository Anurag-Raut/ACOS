#!/usr/bin/env python3
"""Unpack Android packed relocations (APS2 / ANDROID_RELA) and scan for
JNINativeMethod tables in an Android x86_64 shared object.

Android .so's are built with `-Wl,--pack-dyn-relocs=android` (readelf shows
section type ANDROID_RELA).  .data.rel.ro is therefore *zero filled* and every
pointer lives as an R_X86_64_RELATIVE addend in the packed relocation stream.
So: unpack relocations -> build offset->value map -> validate 24-byte
{name,sig,fn} triples.
"""
import sys, json, struct

def u16(b, o): return struct.unpack_from('<H', b, o)[0]
def u32(b, o): return struct.unpack_from('<I', b, o)[0]
def u64(b, o): return struct.unpack_from('<Q', b, o)[0]

class Reader:
    def __init__(self, buf, pos=0):
        self.b = buf; self.p = pos
    def sleb(self):
        result = 0; shift = 0
        while True:
            byte = self.b[self.p]; self.p += 1
            result |= (byte & 0x7f) << shift
            shift += 7
            if not (byte & 0x80):
                if byte & 0x40:
                    result -= 1 << shift
                return result
    def uleb(self):
        result = 0; shift = 0
        while True:
            byte = self.b[self.p]; self.p += 1
            result |= (byte & 0x7f) << shift
            shift += 7
            if not (byte & 0x80):
                return result

GROUPED_BY_INFO = 1
GROUPED_BY_OFFSET_DELTA = 2
GROUPED_BY_ADDEND = 4
HAS_ADDEND = 8

def unpack_relocations(data):
    """-> list of (offset, type, addend)"""
    r = Reader(data)
    magic = data[:4]
    assert magic == b'APS2', magic
    r.p = 4
    relocs = []
    while r.p < len(data):
        group_size = r.sleb()
        flags = r.sleb()
        if group_size <= 0:
            break
        if flags & GROUPED_BY_INFO:
            r_info = r.uleb()
        if flags & GROUPED_BY_OFFSET_DELTA:
            offset_delta = r.sleb()
        if flags & GROUPED_BY_ADDEND:
            addend = r.sleb()
        offset = 0
        for _ in range(group_size):
            if flags & GROUPED_BY_OFFSET_DELTA:
                offset += offset_delta
            else:
                offset += r.sleb()
            if not flags & GROUPED_BY_INFO:
                r_info = r.uleb()
            if flags & HAS_ADDEND:
                if not flags & GROUPED_BY_ADDEND:
                    addend = r.sleb()
                a = addend
            else:
                a = 0
            relocs.append((offset, r_info & 0xffffffff, a))
    return relocs

class Elf:
    def __init__(self, path):
        self.b = open(path, 'rb').read()
        b = self.b
        e_shoff = u64(b, 0x28); e_shentsize = u16(b, 0x3a)
        e_shnum = u16(b, 0x3c); e_shstrndx = u16(b, 0x3e)
        secs = []
        for i in range(e_shnum):
            o = e_shoff + i * e_shentsize
            secs.append(dict(name_off=u32(b, o), type=u32(b, o + 4), flags=u64(b, o + 8),
                             addr=u64(b, o + 0x10), off=u64(b, o + 0x18),
                             size=u64(b, o + 0x20)))
        shstr = secs[e_shstrndx]
        for s in secs:
            c = b[shstr['off'] + s['name_off']:]
            s['name'] = c[:c.index(b'\0')].decode()
        self.secs = secs

    def by_name(self, n):
        for s in self.secs:
            if s['name'] == n:
                return s
        return None

    def vaddr_to(self, va):
        for s in self.secs:
            if s['type'] in (8, 7, 0) or s['size'] == 0:
                continue
            if s['addr'] <= va < s['addr'] + s['size']:
                return s, va - s['addr']
        return None, None

    def vaddr_to_file(self, file_off):
        for s in self.secs:
            if s['type'] in (8, 7, 0) or s['size'] == 0:
                continue
            if s['off'] <= file_off < s['off'] + s['size']:
                return s, file_off - s['off']
        return None, None

    def cstr(self, va, limit=200):
        s, off = self.vaddr_to(va)
        if s is None:
            return None
        start = s['off'] + off
        end = self.b.find(b'\0', start, start + limit)
        if end < 0:
            return None
        try:
            return self.b[start:end].decode('ascii')
        except UnicodeDecodeError:
            return None


R_X86_64_RELATIVE = 8
R_X86_64_64 = 1

def build_ptr_map(e):
    """offset (file-relative in section address space? -> use vaddr) -> value"""
    ptr = {}
    rela = e.by_name('.rela.dyn')
    if rela is not None and rela['type'] == 0x60000002:  # SHT_ANDROID_RELA
        data = e.b[rela['off']:rela['off'] + rela['size']]
        try:
            rels = unpack_relocations(data)
        except IndexError:
            rels = []
        for off, typ, add in rels:
            if typ in (R_X86_64_RELATIVE, R_X86_64_64):
                ptr[off] = add & 0xffffffffffffffff
    if not ptr:
        # plain RELA / RELR fallback
        for nm in ('.rela.dyn',):
            s = e.by_name(nm)
            if not s:
                continue
            for i in range(0, s['size'], 24):
                off = u64(e.b, s['off'] + i); info = u64(e.b, s['off'] + i + 8)
                add = u64(e.b, s['off'] + i + 16)
                if (info & 0xffffffff) == R_X86_64_RELATIVE:
                    ptr[off] = add
    return ptr

def read_ptr(e, ptr, va):
    s, off = e.vaddr_to(va)
    if s is None:
        return None
    if va in ptr:
        return ptr[va]
    f = s['off'] + off
    if s['type'] == 1 and s['size']:
        return u64(e.b, f)
    return None

def scan(path):
    e = Elf(path)
    ptr = build_ptr_map(e)
    exec_ranges = [(s['addr'], s['addr'] + s['size']) for s in e.secs
                   if s['flags'] & 0x4 and s['type'] == 1]
    def is_exec(va): return any(a <= va < b for a, b in exec_ranges)
    def is_ro(va):
        s, _ = e.vaddr_to(va)
        return s is not None and (s['flags'] & 0x2) and not (s['flags'] & 0x4) and s['type'] == 1

    tables = []
    for sec in e.secs:
        if sec['type'] != 1 or sec['size'] < 24: continue
        if not (sec['flags'] & 0x2) or (sec['flags'] & 0x4): continue
        if sec['name'] not in ('.data.rel.ro', '.data.rel.ro.local', '.data'): continue
        base, end = sec['off'], sec['off'] + sec['size']
        off = base; cur = None
        while off + 24 <= end:
            va = sec['addr'] + (off - base)
            nv = read_ptr(e, ptr, va)
            sv = read_ptr(e, ptr, va + 8) if nv else None
            fv = read_ptr(e, ptr, va + 16) if nv and sv else None
            ok = False; nm = sg = None
            if nv and sv and fv and is_ro(nv) and is_ro(sv) and is_exec(fv):
                nm = e.cstr(nv); sg = e.cstr(sv) if nm else None
                if (nm and 1 <= len(nm) <= 96 and all(32 < ord(c) < 127 for c in nm)
                        and sg and sg.startswith('(') and ')' in sg and len(sg) >= 3
                        and sg[sg.rindex(')') + 1:] in ('V', 'Z', 'B', 'C', 'S', 'I', 'J',
                                                        'F', 'D', 'L', '[') and
                        (sg[sg.rindex(')') + 1:].startswith('L') is False or
                         sg.endswith(';'))):
                    ok = True
            if ok:
                if cur is None:
                    cur = dict(section=sec['name'], addr=va, entries=[])
                    tables.append(cur)
                cur['entries'].append(dict(name=nm, sig=sg, fn=fv))
                off += 24
            else:
                cur = None; off += 8
    return e, ptr, tables

def main():
    allout = {}
    for path in sys.argv[1:]:
        e, ptr, tabs = scan(path)
        name = path.split('/')[-1]
        tot = sum(len(t['entries']) for t in tabs)
        print(f"== {name}: {len(tabs)} tables, {tot} natives, relocs={len(ptr)}")
        for t in tabs:
            print(f"   {t['section']}@{t['addr']:#x} n={len(t['entries'])}: "
                  + ", ".join(x['name'] for x in t['entries'][:6])
                  + (" ..." if len(t['entries']) > 6 else ""))
        allout[name] = tabs
    json.dump(allout, open('/home/Qui/Projects/acos/.cache/art-exp-e/jninatives.json', 'w'), indent=1)
    print("wrote .cache/art-exp-e/jninatives.json")

if __name__ == '__main__':
    main()
