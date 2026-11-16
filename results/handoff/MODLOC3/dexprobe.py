import importlib.util, struct, sys
spec = importlib.util.spec_from_file_location("dexclosure", "research/framework-dex/dexclosure.py")
m = importlib.util.module_from_spec(spec); spec.loader.exec_module(m)
SIZES = m.load_opcode_sizes("crates/acos-dex/src/opcode.rs")

def decode(d, insns):
    pos, out = 0, []
    while pos + 2 <= len(insns):
        unit = struct.unpack_from("<H", insns, pos)[0]
        op = unit & 0xff
        if unit == 0x0100:
            size = struct.unpack_from("<H", insns, pos+2)[0] * 2 + 4
        elif unit == 0x0200:
            size = struct.unpack_from("<H", insns, pos+2)[0] * 4 + 2
        elif unit == 0x0300:
            width = struct.unpack_from("<H", insns, pos+2)[0]
            count = struct.unpack_from("<I", insns, pos+4)[0]
            size = ((width*count)+1)//2 + 4
        else:
            size = SIZES.get(op, 1)
            out.append((pos//2, op, insns[pos:pos+2*size]))
        pos += 2*size
    return out

path, cls_want, meth_want = sys.argv[1], sys.argv[2], sys.argv[3]
d = m.DexFile(path, open(path,'rb').read())
for i, vals in enumerate(d.class_defs_full()):
    klass = d.type_desc(vals[0])
    if cls_want not in klass: continue
    direct, virt = d.class_data(vals[6])
    for (midx, acc, code_off) in direct + virt:
        c, name, proto = d.method_ref(midx)
        if name != meth_want: continue
        print("CLASS", klass); print("METHOD", name, proto)
        for (pc, op, raw) in decode(d, d.insns(code_off)):
            desc = ""
            if op in (0x1a, 0x1b):
                sidx = struct.unpack_from("<H", raw, 2)[0] if op==0x1a else struct.unpack_from("<I", raw, 2)[0]
                try: desc = '"%s"' % d.string(sidx)
                except Exception: desc = "str#%d" % sidx
            elif 0x6e <= op <= 0x72 or op == 0x74:
                mid = struct.unpack_from("<H", raw, 2)[0]
                try: desc = "%s->%s%s" % d.method_ref(mid)
                except Exception: desc = "meth#%d" % mid
            elif op in (0x22, 0x23):  # new-instance / new-array
                try: desc = d.type_desc(struct.unpack_from("<H", raw, 2)[0])
                except Exception: pass
            print("  %04x op=%02x %s" % (pc, op, desc))
