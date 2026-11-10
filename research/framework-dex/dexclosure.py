#!/usr/bin/env python3
"""Standard-library DEX inventory + conservative static reachability closure.

Purpose: produce FACTS for the ACOS "run the real Android framework DEX on a
Rust VM" hypothesis.  It parses raw DEX files directly (no external deps) and:

  inventory <dex...>            per-dex class/method/native/<clinit> counts
  closure --roots <spec> <dex...>  reachable closure (classes/methods/natives)

It decodes only the instruction classes needed for a conservative call graph:
invoke-*, static/instance field access (class-init triggers) and new-instance.
Opcode sizes come from the Dalvik spec table mirrored in
crates/acos-dex/src/opcode.rs (parsed so we do not duplicate it by hand).

Approximation (stated in the report): invoke-virtual/interface edges include the
declared method plus every same-name+proto method declared by a (transitive)
subclass / implementor of the declaring class, computed from all DEX files given
on the command line.  This over-approximates dynamic dispatch.  Class
initialisers are added for every class whose static method/field is touched and
for every invoked method's declaring class.
"""

import os
import re
import struct
import sys
from collections import defaultdict

# ---------------------------------------------------------------------------
# Opcode sizes (parsed from crates/acos-dex/src/opcode.rs)
# ---------------------------------------------------------------------------

FORMAT_SIZE = {
    "F10x": 1, "F12x": 1, "F11n": 1, "F11x": 1, "F10t": 1,
    "F20t": 2, "F22x": 2, "F21t": 2, "F21s": 2, "F21h": 2, "F21c": 2,
    "F23x": 2, "F22b": 2, "F22t": 2, "F22s": 2, "F22c": 2,
    "F30t": 3, "F32x": 3, "F31i": 3, "F31t": 3, "F31c": 3, "F35c": 3, "F3rc": 3,
    "F45cc": 4, "F4rcc": 4, "F51l": 5,
}


def load_opcode_sizes(opcode_rs):
    """Return {opcode:int -> size_in_16bit_units} from the Rust table."""
    sizes = {}
    pat = re.compile(r'\(0x([0-9a-fA-F]{2}),\s*"([^"]+)",\s*Format::(F\w+)\)')
    with open(opcode_rs, "r", encoding="utf-8") as fh:
        for line in fh:
            m = pat.search(line)
            if m:
                sizes[int(m.group(1), 16)] = FORMAT_SIZE[m.group(3)]
    if len(sizes) < 200:
        raise SystemExit("opcode table parse failed (%d entries)" % len(sizes))
    return sizes


# ---------------------------------------------------------------------------
# DEX parsing
# ---------------------------------------------------------------------------

def uleb128(buf, off):
    result = 0
    shift = 0
    while True:
        b = buf[off]
        off += 1
        result |= (b & 0x7F) << shift
        if not (b & 0x80):
            break
        shift += 7
    return result, off


class DexFile:
    def __init__(self, path, data):
        self.path = path
        self.data = data
        (self.file_size, self.header_size, self.endian) = struct.unpack_from("<III", data, 32)
        if data[:4] != b"dex\n":
            raise ValueError("not a dex: %s" % path)
        self.version = data[4:7].decode("ascii")
        (self.string_ids_size, self.string_ids_off,
         self.type_ids_size, self.type_ids_off,
         self.proto_ids_size, self.proto_ids_off,
         self.field_ids_size, self.field_ids_off,
         self.method_ids_size, self.method_ids_off,
         self.class_defs_size, self.class_defs_off) = struct.unpack_from("<12I", data, 56)
        self._strings = {}
        self._string_data_off = struct.unpack_from("<%dI" % self.string_ids_size,
                                                   data, self.string_ids_off) if self.string_ids_size else ()

    def string(self, idx):
        if idx in self._strings:
            return self._strings[idx]
        off = self._string_data_off[idx]
        _, off = uleb128(self.data, off)  # utf16 length
        end = self.data.index(b"\x00", off)
        raw = self.data[off:end]
        # minimal MUTF-8 decode (0xC0 0x80 == NUL)
        if b"\xc0\x80" in raw:
            raw = raw.replace(b"\xc0\x80", b"\x00")
        s = raw.decode("utf-8", "replace")
        self._strings[idx] = s
        return s

    def type_desc(self, idx):
        return self.string(struct.unpack_from("<I", self.data, self.type_ids_off + 4 * idx)[0])

    def proto_sig(self, idx):
        shorty, ret, params_off = struct.unpack_from("<III", self.data,
                                                     self.proto_ids_off + 12 * idx)
        parts = []
        if params_off:
            n = struct.unpack_from("<I", self.data, params_off)[0]
            for i in range(n):
                t = struct.unpack_from("<H", self.data, params_off + 4 + 2 * i)[0]
                parts.append(self.type_desc(t))
        return "(" + "".join(parts) + ")" + self.type_desc(ret)

    def field_ref(self, idx):
        c, t, n = struct.unpack_from("<HHI", self.data, self.field_ids_off + 8 * idx)
        return (self.type_desc(c), self.string(n))

    def method_ref(self, idx):
        c, p, n = struct.unpack_from("<HHI", self.data, self.method_ids_off + 8 * idx)
        return (self.type_desc(c), self.string(n), self.proto_sig(p))

    def class_defs_full(self):
        for i in range(self.class_defs_size):
            vals = struct.unpack_from("<8I", self.data, self.class_defs_off + 32 * i)
            yield vals  # class_idx, access, superclass_idx, interfaces_off,
                        # source_file_idx, annotations_off, class_data_off, static_values_off

    def interfaces(self, off):
        if not off:
            return []
        n = struct.unpack_from("<I", self.data, off)[0]
        return [struct.unpack_from("<H", self.data, off + 4 + 2 * i)[0] for i in range(n)]

    def class_data(self, off):
        """Return (direct_methods, virtual_methods) as lists of
        (method_idx, access_flags, code_off)."""
        if not off:
            return [], []
        sf, sf_off = uleb128(self.data, off)
        inf, off2 = uleb128(self.data, sf_off)
        dm, off3 = uleb128(self.data, off2)
        vm, off4 = uleb128(self.data, off3)
        # skip fields
        for _ in range(sf + inf):
            _, off4 = uleb128(self.data, off4)
            _, off4 = uleb128(self.data, off4)
        direct = []
        idx = 0
        for _ in range(dm):
            diff, off4 = uleb128(self.data, off4)
            acc, off4 = uleb128(self.data, off4)
            code, off4 = uleb128(self.data, off4)
            idx += diff
            direct.append((idx, acc, code))
        virt = []
        idx = 0
        for _ in range(vm):
            diff, off4 = uleb128(self.data, off4)
            acc, off4 = uleb128(self.data, off4)
            code, off4 = uleb128(self.data, off4)
            idx += diff
            virt.append((idx, acc, code))
        return direct, virt

    def insns(self, code_off):
        if not code_off:
            return b""
        insns_size = struct.unpack_from("<I", self.data, code_off + 12)[0]
        start = code_off + 16
        return self.data[start:start + insns_size * 2]


# ---------------------------------------------------------------------------
# Global boot-classpath model
# ---------------------------------------------------------------------------

class Model:
    def __init__(self, opcode_sizes):
        self.op_sizes = opcode_sizes
        self.methods = []          # idx -> (class_desc, name, proto, access, code_units, dex)
        self.method_index = {}     # (class, name, proto) -> mid
        self.classes = {}          # desc -> dict(super, ifaces, methods=[mid])
        self.static_fields = {}    # (class_desc, field_name) -> owner class
        self.raw = []              # decoded insn bytes per mid (lazy)

    def add_dex(self, dex):
        for vals in dex.class_defs_full():
            class_idx, access, super_idx, ifaces_off, _src, _ann, cdata_off, _sv = vals
            desc = dex.type_desc(class_idx)
            supers = dex.type_desc(super_idx) if super_idx != 0xFFFFFFFF else None
            ifaces = [dex.type_desc(i) for i in dex.interfaces(ifaces_off)]
            cls = self.classes.setdefault(desc, {"super": supers, "ifaces": set(), "methods": []})
            cls["access"] = access
            if supers:
                cls["super"] = supers
            cls["ifaces"].update(ifaces)
            direct, virt = dex.class_data(cdata_off)
            for mid, acc, code in direct + virt:
                c, name, proto = dex.method_ref(mid)
                # method_ids already carry the defining class; trust it
                key = (c, name, proto)
                gid = self.method_index.get(key)
                if gid is None:
                    gid = len(self.methods)
                    self.method_index[key] = gid
                    self.methods.append([c, name, proto, acc, dex.insns(code), dex])
                else:
                    # duplicate definition: prefer the one with code
                    if not self.methods[gid][4] and code:
                        self.methods[gid] = [c, name, proto, acc, dex.insns(code), dex]
                self.classes.setdefault(c, {"super": None, "ifaces": set(), "methods": []})
                self.classes[c]["methods"].append(gid)

    # ---------------- graph ----------------
    def build(self):
        # direct subclasses / implementors
        self.sub = defaultdict(set)
        self.impl = defaultdict(set)   # interface desc -> direct implementors
        for desc, c in self.classes.items():
            if c["super"]:
                self.sub[c["super"]].add(desc)
            for i in c["ifaces"]:
                self.impl[i].add(desc)
        self.sub_closure = {}
        self.impl_closure = {}

        def subs(d):
            if d not in self.sub_closure:
                acc = set()
                stack = list(self.sub.get(d, ()))
                while stack:
                    x = stack.pop()
                    if x in acc:
                        continue
                    acc.add(x)
                    stack.extend(self.sub.get(x, ()))
                self.sub_closure[d] = acc
            return self.sub_closure[d]

        def impls(d):
            if d not in self.impl_closure:
                acc = set()
                stack = list(self.impl.get(d, ()))
                while stack:
                    x = stack.pop()
                    if x in acc:
                        continue
                    acc.add(x)
                    stack.extend(self.impl.get(x, ()))
                    # subclasses of implementors also implement it
                    stack.extend(self.sub.get(x, ()))
                    # if x implements another interface that extends d, ignore
                self.impl_closure[d] = acc
            return self.impl_closure[d]

        self._subs = subs
        self._impls = impls
        self.by_nameproto = defaultdict(set)
        for mid, (c, name, proto, acc, ins, dex) in enumerate(self.methods):
            self.by_nameproto[(name, proto)].add(mid)
        self._virt_cache = {}
        self._edges = {}

    def virtual_targets(self, mid):
        if mid in self._virt_cache:
            return self._virt_cache[mid]
        c, name, proto, acc, ins, dex = self.methods[mid]
        if VIRT_OFF:
            self._virt_cache[mid] = [mid]
            return self._virt_cache[mid]
        cand = self.by_nameproto.get((name, proto), ())
        out = []
        for m in cand:
            dc = self.methods[m][0]
            if dc == c or dc in self._subs(c):
                out.append(m)
        self._virt_cache[mid] = out
        return out

    def interface_targets(self, mid):
        if mid in self._virt_cache:
            return self._virt_cache[mid]
        c, name, proto, acc, ins, dex = self.methods[mid]
        if VIRT_OFF:
            self._virt_cache[mid] = [mid]
            return self._virt_cache[mid]
        cand = self.by_nameproto.get((name, proto), ())
        out = []
        for m in cand:
            dc = self.methods[m][0]
            if dc == c or dc in self._impls(c):
                out.append(m)
        self._virt_cache[mid] = out
        return out

    def resolve_method(self, dex, local_idx):
        """Map a DEX-local method index to a global method id (or None)."""
        try:
            c, name, proto = dex.method_ref(local_idx)
        except Exception:
            return None
        return self.method_index.get((c, name, proto))

    def clinit_of(self, class_desc):
        return self.method_index.get((class_desc, "<clinit>", "()V"))

    def callees(self, mid):
        if mid in self._edges:
            return self._edges[mid]
        out = set()
        cdesc, name, proto, acc, ins, dex = self.methods[mid]
        # entering a static method initialises its class (over-approx: all methods)
        cl = self.clinit_of(cdesc)
        if cl is not None and cl != mid:
            out.add(cl)
        sizes = self.op_sizes
        n = len(ins)
        i = 0
        payloads = {}
        while i + 2 <= n:
            if i in payloads:
                i += payloads[i]
                continue
            op = ins[i]
            size = sizes.get(op)
            if size is None:
                # no complete table: cannot safely continue
                break
            if op in (0x26, 0x2B, 0x2C) and i + 6 <= n:
                # F31t: 32-bit signed branch to an inline payload
                off = struct.unpack_from("<i", ins, i + 2)[0]
                po = i + off * 2
                if 0 <= po + 8 <= n:
                    ident = ins[po] | (ins[po + 1] << 8)
                    if ident == 0x0300:
                        width = ins[po + 4] | (ins[po + 5] << 8)
                        cnt = struct.unpack_from("<I", ins, po + 6)[0]
                        blen = 8 + cnt * width
                    elif ident == 0x0100:
                        cnt = ins[po + 2] | (ins[po + 3] << 8)
                        blen = 8 + 4 * cnt
                    elif ident == 0x0200:
                        cnt = ins[po + 2] | (ins[po + 3] << 8)
                        blen = 4 + 8 * cnt
                    else:
                        blen = 0
                    if blen:
                        blen += blen & 1
                        payloads[po] = blen
            if size == 1:
                i += 2
                continue
            u1 = ins[i + 1]
            # second code unit (little-endian) is the index for all formats we need
            idx = ins[i + 2] | (ins[i + 3] << 8) if i + 4 <= n else 0
            if 0x6E <= op <= 0x72 or 0x74 <= op <= 0x78:
                # idx is a DEX-local method index; resolve to global method id
                tgt = self.resolve_method(dex, idx)
                if tgt is not None:
                    if op in (0x6E, 0x74):       # invoke-virtual
                        out.update(self.virtual_targets(tgt))
                    elif op in (0x72, 0x78):     # invoke-interface
                        out.update(self.interface_targets(tgt))
                    elif op in (0x6F, 0x75):     # invoke-super
                        out.add(tgt)
                        sc = self.classes.get(self.methods[tgt][0], {}).get("super")
                        while sc:
                            cand = self.method_index.get((sc, name, proto))
                            if cand is not None:
                                out.add(cand)
                                break
                            sc = self.classes.get(sc, {}).get("super")
                    else:                        # invoke-direct/static (& range)
                        out.add(tgt)
                        # class init of target's owning class
                        cl2 = self.clinit_of(self.methods[tgt][0])
                        if cl2 is not None and cl2 != mid:
                            out.add(cl2)
            elif op == 0x27:                     # throw: exception classes
                pass
            elif op == 0x1C:                     # const-class: load only
                dex.type_desc(idx)
            elif op == 0x22:                      # new-instance
                td = dex.type_desc(idx)
                out.update(self._class_init_chain(td))
            elif 0x60 <= op <= 0x6D:              # sget*/sput*
                fc, fn = dex.field_ref(idx)
                out.update(self._class_init_chain(fc))
            # 0x1c const-class: loads but does not initialise
            i += size * 2
        self._edges[mid] = out
        return out

    def _class_init_chain(self, desc):
        out = []
        seen = set()
        while desc and desc not in seen:
            seen.add(desc)
            cl = self.clinit_of(desc)
            if cl is not None:
                out.append(cl)
            desc = self.classes.get(desc, {}).get("super")
        return out

    def closure(self, roots):
        seen = set()
        stack = list(roots)
        while stack:
            m = stack.pop()
            if m in seen:
                continue
            seen.add(m)
            stack.extend(self.callees(m))
        return seen


# ---------------------------------------------------------------------------
# helpers
# ---------------------------------------------------------------------------

VIRT_OFF = os.environ.get("VIRT", "1") == "0"

CORE_PREFIXES = ("Ljava/", "Ljavax/", "Lsun/", "Ldalvik/system/", "Llibcore/",
                 "Lorg/apache/harmony/", "Lcom/android/org/conscrypt/",
                 "Landroid/icu/", "Lcom/android/icu/", "Lcom/android/i18n/")


def is_core(desc):
    return desc.startswith(CORE_PREFIXES)


def klass(desc):
    return desc.split("->")[0] if "->" in desc else desc


def main():
    here = os.path.dirname(os.path.abspath(__file__))
    opcode_rs = os.path.join(here, "..", "..", "crates", "acos-dex", "src", "opcode.rs")
    opcode_rs = os.path.abspath(opcode_rs)
    op_sizes = load_opcode_sizes(opcode_rs)

    mode = sys.argv[1]
    if mode == "inventory":
        paths = sys.argv[2:]
        for p in paths:
            data = open(p, "rb").read()
            dex = DexFile(p, data)
            natives = 0
            clinits = 0
            methods = 0
            for vals in dex.class_defs_full():
                cdata_off = vals[6]
                direct, virt = dex.class_data(cdata_off)
                for mid, acc, code in direct + virt:
                    methods += 1
                    if acc & 0x100:
                        natives += 1
                    _, name, _ = dex.method_ref(mid)
                    if name == "<clinit>":
                        clinits += 1
            print("%s version=%s classes=%d methods=%d native=%d clinit=%d size=%d"
                  % (p, dex.version, dex.class_defs_size, methods, natives, clinits,
                     len(data)))
        return

    if mode == "jarinv":
        paths = sys.argv[2:]
        classes = methods = natives = clinits = 0
        total = 0
        versions = set()
        native_by_class = defaultdict(int)
        clinit_classes = set()
        dex_count = 0
        for p in paths:
            data = open(p, "rb").read()
            dex = DexFile(p, data)
            dex_count += 1
            total += len(data)
            versions.add(dex.version)
            classes += dex.class_defs_size
            for vals in dex.class_defs_full():
                desc = dex.type_desc(vals[0])
                cdata_off = vals[6]
                direct, virt = dex.class_data(cdata_off)
                for mid, acc, code in direct + virt:
                    methods += 1
                    _, name, _ = dex.method_ref(mid)
                    if acc & 0x100:
                        natives += 1
                        native_by_class[desc] += 1
                    if name == "<clinit>":
                        clinits += 1
                        clinit_classes.add(desc)
        print("DEX_FILES %d" % dex_count)
        print("TOTAL_SIZE %d" % total)
        print("DEX_VERSIONS %s" % ",".join(sorted(versions)))
        print("CLASSES %d" % classes)
        print("METHODS %d" % methods)
        print("NATIVE_METHODS %d" % natives)
        print("CLINIT_METHODS %d" % clinits)
        print("CLASSES_WITH_CLINIT %d" % len(clinit_classes))
        for c, n in sorted(native_by_class.items(), key=lambda x: (-x[1], x[0]))[:40]:
            print("TOPNATIVE\t%d\t%s" % (n, c))
        if os.environ.get("DUMP_CLINIT"):
            for c in sorted(clinit_classes):
                print("CLINIT\t%s" % c)
        return

    if mode == "closure":
        # argv: closure [--roots R1,R2,...] dex...
        roots_spec = None
        args = sys.argv[2:]
        if args and args[0] == "--roots":
            roots_spec = args[1]
            args = args[2:]
        model = Model(op_sizes)
        for p in args:
            dex = DexFile(p, open(p, "rb").read())
            model.add_dex(dex)
        model.build()
        roots = []
        for spec in roots_spec.split("||"):
            spec = spec.strip()
            if not spec:
                continue
            if spec.endswith("*"):
                prefix = spec[:-1]
                for (c, name, proto), mid in model.method_index.items():
                    if ("%s->%s%s" % (c, name, proto)).startswith(prefix):
                        roots.append(mid)
            else:
                c, rest = spec.split("->", 1)
                name = rest.split("(", 1)[0]
                proto = "(" + rest.split("(", 1)[1]
                mid = model.method_index.get((c, name, proto))
                if mid is None:
                    print("ROOT-NOT-FOUND %s" % spec, file=sys.stderr)
                else:
                    roots.append(mid)
        seen = model.closure(roots)
        # clinit of println roots etc already included via callees
        classes = set()
        natives = []
        bytecode = 0
        for m in seen:
            c, name, proto, acc, ins, dex = model.methods[m]
            classes.add(c)
            bytecode += len(ins)
            if acc & 0x100:
                natives.append((c, name, proto))
        print("ROOTS %d" % len(roots))
        print("REACHABLE_METHODS %d" % len(seen))
        print("REACHABLE_CLASSES %d" % len(classes))
        print("REACHABLE_BYTECODE_BYTES %d" % bytecode)
        print("REACHABLE_NATIVE %d" % len(natives))
        core = [x for x in natives if is_core(x[0])]
        andr = [x for x in natives if x[0].startswith("Landroid/")]
        other = [x for x in natives if x not in core and x not in andr]
        print("REACHABLE_NATIVE_CORE %d" % len(core))
        print("REACHABLE_NATIVE_ANDROID %d" % len(andr))
        print("REACHABLE_NATIVE_OTHER %d" % len(other))
        if os.environ.get("DUMP_NATIVES"):
            for x in sorted(natives):
                print("NATIVE\t%s\t%s\t%s" % x)
        return

    if mode == "aidl":
        roots_spec = None
        args = sys.argv[2:]
        if args and args[0] == "--roots":
            roots_spec = args[1]
            args = args[2:]
        VIRT = os.environ.get("VIRT", "0") == "1"
        model = Model(op_sizes)
        for p in args:
            model.add_dex(DexFile(p, open(p, "rb").read()))
        model.build()
        roots = []
        for spec in roots_spec.split("||"):
            spec = spec.strip()
            if not spec:
                continue
            c, rest = spec.split("->", 1)
            name = rest.split("(", 1)[0]
            proto = "(" + rest.split("(", 1)[1]
            mid = model.method_index.get((c, name, proto))
            if mid is None:
                print("ROOT-NOT-FOUND %s" % spec, file=sys.stderr)
            else:
                roots.append(mid)
        seen = model.closure(roots)
        # scan invokes whose declaring class is an interface
        found = defaultdict(list)
        for mid in seen:
            cdesc, name, proto, acc, ins, dex = model.methods[mid]
            i = 0
            while i + 4 <= len(ins):
                op = ins[i]
                sz = model.op_sizes.get(op)
                if sz is None:
                    break
                if 0x6E <= op <= 0x72 or 0x74 <= op <= 0x78:
                    lidx = ins[i + 2] | (ins[i + 3] << 8)
                    try:
                        dc, dn, dp = dex.method_ref(lidx)
                    except Exception:
                        dc = None
                    if dc and model.classes.get(dc, {}).get("access", 0) & 0x200:
                        found[dc].append((dn + dp, "%s->%s%s" % (cdesc, name, proto)))
                i += sz * 2
        print("INTERFACES_IN_CLOSURE %d" % len(found))
        for dc in sorted(found):
            calls = found[dc]
            meths = sorted(set(x[0] for x in calls))
            callers = sorted(set(x[1] for x in calls))
            print("IFACE\t%s\tcalls=%d\tmethods=%s" % (dc, len(calls), ",".join(meths[:25])))
            for cr in callers[:12]:
                print("\tCALLER\t%s" % cr)
        return

    if mode == "dump-model":
        # debug helper: dump-model <query> <dex...>
        q = sys.argv[2]
        model = Model(op_sizes)
        for p in sys.argv[3:]:
            model.add_dex(DexFile(p, open(p, "rb").read()))
        model.build()
        for (c, name, proto), mid in sorted(model.method_index.items()):
            if q in c:
                print("%s->%s%s" % (c, name, proto))
        return

    raise SystemExit("usage: inventory <dex...> | closure [--roots ...] <dex...>")


if __name__ == "__main__":
    main()
