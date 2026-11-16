import importlib.util, struct, sys, re

spec = importlib.util.spec_from_file_location("dexclosure", "research/framework-dex/dexclosure.py")
m = importlib.util.module_from_spec(spec); spec.loader.exec_module(m)

table = {}
for line in open("crates/acos-dex/src/opcode.rs"):
    mm = re.match(r'\s*\(0x([0-9a-f]{2}), "([^"]+)", Format::(F[0-9a-z]+)\),', line)
    if mm:
        table[int(mm.group(1), 16)] = (mm.group(2), mm.group(3))

SIZE = {'F10x':1,'F12x':1,'F11n':1,'F11x':1,'F10t':1,'F20t':2,'F22x':2,'F21t':2,'F21s':2,
 'F21h':2,'F21c':2,'F23x':2,'F22b':2,'F22t':2,'F22s':2,'F22c':2,'F30t':3,'F32x':3,
 'F31i':3,'F31t':3,'F31c':3,'F35c':3,'F3rc':3,'F51l':5,'F45cc':4,'F4rcc':4}

def s16(u): return u-0x10000 if u & 0x8000 else u
def s32(u): return u-0x100000000 if u & 0x80000000 else u

def decode(d, insns):
    pos = 0; out = []
    while pos + 2 <= len(insns):
        pc = pos // 2
        op = struct.unpack_from("<H", insns, pos)[0] & 0xff
        name, fmt = table.get(op, ("UNASSIGNED%02x" % op, 'F10x'))
        size = SIZE.get(fmt, 1)
        w = [struct.unpack_from("<H", insns, pos+2*i)[0] for i in range(size)]
        raw = insns[pos:pos+2*size]
        A = (w[0]>>8)&0xf
        B = (w[0]>>12)&0xf
        AA = (w[0]>>8)&0xff
        text = ""; ref = None; reftype = None
        try:
            if fmt == 'F10x': text = ""
            elif fmt == 'F12x': text = "v%d, v%d" % (A, B)
            elif fmt == 'F11n':
                lit = B-16 if B & 8 else B
                text = "v%d, #%d" % (A, lit)
            elif fmt == 'F11x': text = "v%d" % AA
            elif fmt == 'F10t':
                off = AA-0x100 if AA & 0x80 else AA
                text = "%+d (-> %04x)" % (off, pc+off)
            elif fmt == 'F20t': text = "%+d (-> %04x)" % (s16(w[1]), pc+s16(w[1]))
            elif fmt == 'F22x': text = "v%d, v%d" % (AA, w[1])
            elif fmt == 'F21t': text = "v%d, %+d (-> %04x)" % (AA, s16(w[1]), pc+s16(w[1]))
            elif fmt == 'F21s': text = "v%d, #%d" % (AA, s16(w[1]))
            elif fmt == 'F21h': text = "v%d, #%d" % (AA, s32(w[1]<<16))
            elif fmt == 'F21c': text = "v%d, @%d" % (AA, w[1]); ref = w[1]
            elif fmt == 'F23x': text = "v%d, v%d, v%d" % (AA, w[1]&0xff, (w[1]>>8)&0xff)
            elif fmt == 'F22b': text = "v%d, v%d, #%d" % (A, w[1]&0xff, (w[1]>>8)&0xff)
            elif fmt == 'F22t': text = "v%d, v%d, %+d (-> %04x)" % (A, B, s16(w[1]), pc+s16(w[1]))
            elif fmt == 'F22s': text = "v%d, v%d, #%d" % (A, B, s16(w[1]))
            elif fmt == 'F22c': text = "v%d, v%d, @%d" % (A, B, w[1]); ref = w[1]
            elif fmt == 'F30t':
                off = s32((w[1]<<16)|w[2]); text = "%+d (-> %04x)" % (off, pc+off)
            elif fmt == 'F32x': text = "v%d, v%d" % (w[1], w[2])
            elif fmt == 'F31i': text = "v%d, #%d" % (AA, s32((w[1]<<16)|w[2]))
            elif fmt == 'F31t': text = "v%d, %+d (-> %04x)" % (AA, s32((w[1]<<16)|w[2]), pc+s32((w[1]<<16)|w[2]))
            elif fmt == 'F31c':
                ref = (w[1]<<16)|w[2]; reftype='string'
                text = "v%d, string@%d" % (AA, ref)
            elif fmt == 'F35c':
                count = (w[0]>>12)&0xf; g = (w[0]>>8)&0xf
                regs = [w[2]&0xf, (w[2]>>4)&0xf, (w[2]>>8)&0xf, (w[2]>>12)&0xf, g][:count]
                ref = w[1]; reftype='method'
                text = "{%s}, method@%d" % (", ".join("v%d" % r for r in regs), ref)
            elif fmt == 'F3rc':
                count = AA; first = w[2]; ref = w[1]; reftype='method'
                text = "{v%d..v%d}, method@%d" % (first, first+count-1, ref)
            elif fmt == 'F51l': text = "v%d, #%d" % (AA, struct.unpack_from("<q", raw, 2)[0])
            elif fmt == 'F45cc':
                count = (w[0]>>12)&0xf; g=(w[0]>>8)&0xf
                regs=[w[2]&0xf,(w[2]>>4)&0xf,(w[2]>>8)&0xf,(w[2]>>12)&0xf,g][:count]
                ref=w[1]; reftype='method'
                text = "{%s}, method@%d, proto@%d" % (", ".join("v%d"%r for r in regs), ref, w[3])
        except Exception as e:
            text = "?" + str(e)

        extra = ""
        if reftype == 'method':
            try:
                c, nm, proto = d.method_ref(ref)
                extra = " // %s->%s%s" % (c, nm, proto)
            except Exception: extra = " // method@%d?" % ref
        elif ref is not None and fmt == 'F31c':
            try: extra = ' // "%s"' % d.string(ref)
            except Exception: pass
        elif ref is not None:
            try:
                extra = " // field %s" % (d.field_ref(ref),)
            except Exception:
                try: extra = " // type %s" % d.type_desc(ref)
                except Exception: extra = " // @%d?" % ref
        out.append((pc, op, name + " " + text + extra))
        pos += 2*size
    return out

path, cls_want, meth_want = sys.argv[1], sys.argv[2], sys.argv[3]
d = m.DexFile(path, open(path,'rb').read())
for i, vals in enumerate(d.class_defs_full()):
    klass = d.type_desc(vals[0])
    if cls_want not in klass: continue
    direct, virt = d.class_data(vals[6])
    for (midx, acc, code_off) in direct + virt:
        c, nm, proto = d.method_ref(midx)
        if nm != meth_want: continue
        print("CLASS", klass); print("METHOD", nm, proto)
        for (pc, op, text) in decode(d, d.insns(code_off)):
            print("  %04x %s" % (pc, text))
