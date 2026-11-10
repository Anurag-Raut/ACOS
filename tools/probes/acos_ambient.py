#!/usr/bin/env python3
"""Shared static-analysis helpers for the ACOS ambient-state probes (T-45 / T-46).

This module is the common engine behind ``prop-keys.py`` (system-property keys
read by the executed framework code) and ``feature-audit.py`` (system features,
services, settings keys, SDK gates and thread names).

Design
------
* The only input to the disassembly is the in-repo ``acos-dex`` example
  ``dexdump`` (``cargo run -p acos-dex --example dexdump``).  Its text form is
  stable and register-rich:

      class LMain;  access=0x0001  super=Ljava/lang/Object;
        method LMain;->main([Ljava/lang/String;)V  access=0x0009  regs=2 ...
          0000: const-string v0, "ro.some.key"
          0002: invoke-static {v0}, Landroid/os/SystemProperties;.get:...
        == verify ==

* The engine walks every method, and for the methods we care about (the
  executed-method sets from ``research/dynamic-trace/data`` plus a small
  supplementary set: ``Build.<clinit>``, ``Build$VERSION.<clinit>``,
  ``SystemServiceRegistry.<clinit>`` and every ``android.sysprop.*`` accessor)
  it follows a small, deterministic register flow:  ``const-string`` /
  ``const`` / ``move`` / ``sget`` / ``sput`` / ``new-instance`` /
  ``StringBuilder`` concatenation / ``invoke`` / ``if`` comparisons.

* It never interprets the native property area: the property *names* come from
  const-strings at call sites, exactly as the report's GAP G-2 describes.

Everything is deterministic: DEX files are visited in a fixed order and every
container is a sorted list at emit time.  Python stdlib only.
"""

from __future__ import annotations

import json
import os
import re
import subprocess
import sys
from collections import OrderedDict

# ---------------------------------------------------------------------------
# paths
# ---------------------------------------------------------------------------


def repo_root(start=None):
    d = os.path.abspath(start or os.path.dirname(__file__))
    while True:
        if os.path.isfile(os.path.join(d, "Cargo.toml")) and os.path.isdir(
            os.path.join(d, "crates", "acos-dex")
        ):
            return d
        parent = os.path.dirname(d)
        if parent == d:
            raise SystemExit("acos_ambient: cannot locate the ACOS repo root")
        d = parent


def default_dirs(root=None):
    root = root or repo_root()
    return {
        "root": root,
        "boot": os.path.join(root, ".cache", "boot", "android-34"),
        "trace": os.path.join(root, "research", "dynamic-trace", "data"),
        "reference": os.path.join(
            root, "fixtures", "reference", "hello-views", "art-api34-x86_64"
        ),
        "out": os.path.join(root, "research", "ambient-state"),
    }


# ---------------------------------------------------------------------------
# dexdump resolution
# ---------------------------------------------------------------------------


def resolve_dexdump(root):
    """Return the argv prefix that invokes the in-repo ``dexdump`` example.

    Honours ``$ACOS_DEXDUMP``.  Prefers a freshly-built release/debug example
    binary; falls back to ``cargo run`` when the source is newer than both.
    """
    env = os.environ.get("ACOS_DEXDUMP")
    if env:
        return env.split()

    src_dir = os.path.join(root, "crates", "acos-dex")
    newest = 0.0
    for dirpath, _dirnames, filenames in os.walk(src_dir):
        for f in filenames:
            if f.endswith(".rs"):
                newest = max(newest, os.path.getmtime(os.path.join(dirpath, f)))
    for rel in ("target/release/examples/dexdump", "target/debug/examples/dexdump"):
        p = os.path.join(root, rel)
        if os.path.isfile(p) and os.access(p, os.X_OK) and os.path.getmtime(p) >= newest:
            return [p]
    return ["cargo", "run", "-q", "-p", "acos-dex", "--example", "dexdump", "--"]


# ---------------------------------------------------------------------------
# disassembly parsing
# ---------------------------------------------------------------------------

_CLASS_RE = re.compile(r"^class (L[^;]+;)\s")
_METHOD_RE = re.compile(
    r"^  method (L[^;]+;)->([^\s(]+)\((.*)\)([^\s]+)\s+access=0x([0-9a-fA-F]+)"
)
_INSN_RE = re.compile(r"^    ([0-9a-f]+): (.*)$")


class Method:
    __slots__ = ("class_desc", "name", "params", "ret", "access", "instrs")

    def __init__(self, class_desc, name, params, ret, access, instrs):
        self.class_desc = class_desc
        self.name = name
        self.params = params
        self.ret = ret
        self.access = access
        self.instrs = instrs

    @property
    def class_dotted(self):
        return class_desc_to_dotted(self.class_desc)

    @property
    def proto(self):
        return "(%s)%s" % (self.params, self.ret)

    def __repr__(self):
        return "%s->%s%s" % (self.class_dotted, self.name, self.proto)


def class_desc_to_dotted(desc):
    if desc.startswith("L") and desc.endswith(";"):
        return desc[1:-1].replace("/", ".")
    return desc


def iter_methods(dexdump_cmd, dex_path):
    """Yield :class:`Method` objects for one DEX, one at a time."""
    proc = subprocess.Popen(
        dexdump_cmd + [dex_path],
        stdout=subprocess.PIPE,
        stderr=subprocess.DEVNULL,
        text=True,
        errors="replace",
    )
    try:
        yield from _parse_disasm(proc.stdout)
    finally:
        try:
            proc.stdout.close()
        except Exception:
            pass
        proc.wait()


def _parse_disasm(lines):
    in_classes = False
    cur_class = None
    cur = None
    instrs = []
    for line in lines:
        if line.startswith("== classes =="):
            in_classes = True
            continue
        if line.startswith("== verify =="):
            break
        if not in_classes:
            continue
        if line.startswith("class "):
            if cur is not None:
                yield cur
                cur = None
            instrs = []
            m = _CLASS_RE.match(line)
            cur_class = m.group(1) if m else None
            continue
        m = _METHOD_RE.match(line)
        if m:
            if cur is not None:
                yield cur
            instrs = []
            cur = Method(
                m.group(1),
                m.group(2),
                m.group(3),
                m.group(4),
                int(m.group(5), 16),
                instrs,
            )
            continue
        m = _INSN_RE.match(line)
        if m and cur is not None:
            instrs.append((int(m.group(1), 16), m.group(2)))
    if cur is not None:
        yield cur


# ---------------------------------------------------------------------------
# opcode formats (read from the in-repo crate so the table never drifts)
# ---------------------------------------------------------------------------

_FORMAT_RE = re.compile(r'\(0x[0-9a-fA-F]+,\s*"([^"]+)",\s*Format::(\w+)\)')

# Formats whose first register operand is a destination (written) register.
DST_FORMATS = {
    "F12x",
    "F22x",
    "F32x",
    "F11n",
    "F11x",
    "F21s",
    "F21h",
    "F31i",
    "F21c",
    "F22c",
    "F51l",
    "F23x",
    "F22b",
    "F22s",
}


def load_format_map(root):
    path = os.path.join(root, "crates", "acos-dex", "src", "opcode.rs")
    out = {}
    with open(path, "r", errors="replace") as f:
        for line in f:
            m = _FORMAT_RE.search(line)
            if m:
                out[m.group(1)] = m.group(2)
    return out


# ---------------------------------------------------------------------------
# instruction field extraction
# ---------------------------------------------------------------------------

_RE_CONST_STR = re.compile(r"^const-string(?:/jumbo)? v(\d+), (.*)$")
_RE_CONST_INT = re.compile(
    r"^const(?:/4|/16|/high16)? v(\d+), #int (-?\d+)$"
)
_RE_CONST_LONG = re.compile(
    r"^const-wide(?:/16|/32|/high16)? v(\d+), #long (-?\d+)$"
)
_RE_MOVE = re.compile(r"^move(?:-[a-z]+)*(?:/from16|/16)? v(\d+)(?:, v(\d+))?$")
_RE_SGET = re.compile(
    r"^sget(?:-object|-wide|-boolean|-byte|-char|-short)? v(\d+), "
    r"(L[^;]+;)\.([^:]+):(\S+)$"
)
_RE_SPUT = re.compile(
    r"^sput(?:-object|-wide|-boolean|-byte|-char|-short)? v(\d+), "
    r"(L[^;]+;)\.([^:]+):(\S+)$"
)
_RE_NEW_INSTANCE = re.compile(r"^new-instance v(\d+), (L[^;]+;)$")
_RE_INVOKE = re.compile(
    r"^(invoke-[a-z-]+(?:/range)?) \{([^}]*)\}, (L[^;]+;)\.([^:]+):(\(.*\))(\S+)$"
)
_RE_IF = re.compile(r"^if-([a-z]+) v(\d+)(?:, v(\d+))?, (\S+)$")
_RE_ANY_REG = re.compile(r"\bv(\d+)\b")


def _strip_strings(text):
    out = []
    i = 0
    n = len(text)
    while i < n:
        c = text[i]
        if c == '"':
            i += 1
            while i < n and text[i] != '"':
                if text[i] == "\\":
                    i += 1
                i += 1
            i += 1
        else:
            out.append(c)
            i += 1
    return "".join(out)


def first_reg(text):
    m = _RE_ANY_REG.search(_strip_strings(text))
    return int(m.group(1)) if m else None


def unescape_rust_string(raw):
    """Undo Rust's ``str`` Debug escaping used by the acos-dex disassembler."""
    out = []
    i = 0
    n = len(raw)
    while i < n:
        c = raw[i]
        if c == "\\" and i + 1 < n:
            e = raw[i + 1]
            if e == "n":
                out.append("\n")
                i += 2
            elif e == "r":
                out.append("\r")
                i += 2
            elif e == "t":
                out.append("\t")
                i += 2
            elif e == "\\":
                out.append("\\")
                i += 2
            elif e == '"':
                out.append('"')
                i += 2
            elif e == "'":
                out.append("'")
                i += 2
            elif e == "0":
                out.append("\0")
                i += 2
            elif e == "u" and i + 2 < n and raw[i + 2] == "{":
                j = raw.index("}", i + 3)
                out.append(chr(int(raw[i + 3 : j], 16)))
                i = j + 1
            else:
                out.append(e)
                i += 2
        else:
            out.append(c)
            i += 1
    return "".join(out)


def parse_const_string(text):
    m = _RE_CONST_STR.match(text)
    if not m:
        return None
    raw = m.group(2)
    if not (raw.startswith('"') and raw.endswith('"') and len(raw) >= 2):
        return None
    return int(m.group(1)), unescape_rust_string(raw[1:-1])


def parse_const_int(text):
    m = _RE_CONST_INT.match(text)
    if m:
        return int(m.group(1)), int(m.group(2))
    m = _RE_CONST_LONG.match(text)
    if m:
        return int(m.group(1)), int(m.group(2))
    return None


def parse_move(text):
    m = _RE_MOVE.match(text)
    if not m:
        return None
    return int(m.group(1)), (int(m.group(2)) if m.group(2) is not None else None)


def parse_sget(text):
    m = _RE_SGET.match(text)
    if not m:
        return None
    return int(m.group(1)), m.group(2), m.group(3), m.group(4)


def parse_sput(text):
    m = _RE_SPUT.match(text)
    if not m:
        return None
    return int(m.group(1)), m.group(2), m.group(3), m.group(4)


def parse_new_instance(text):
    m = _RE_NEW_INSTANCE.match(text)
    if not m:
        return None
    return int(m.group(1)), m.group(2)


def split_params(params):
    """Split the parameter part of a DEX proto (with outer parens)."""
    s = params[1:-1] if params.startswith("(") else params
    out = []
    i = 0
    n = len(s)
    while i < n:
        c = s[i]
        if c == "[":
            j = i
            while j < n and s[j] == "[":
                j += 1
            if j < n and s[j] == "L":
                k = s.index(";", j)
                out.append(s[i : k + 1])
                i = k + 1
            else:
                out.append(s[i : j + 1])
                i = j + 1
        elif c == "L":
            k = s.index(";", i)
            out.append(s[i : k + 1])
            i = k + 1
        else:
            out.append(c)
            i += 1
    return out


def parse_invoke(text):
    m = _RE_INVOKE.match(text)
    if not m:
        return None
    mnemonic = m.group(1)
    raw_regs = m.group(2).strip()
    regs = []
    if ".." in raw_regs:
        a, b = raw_regs.split("..")
        regs = list(range(int(a.strip().lstrip("v")), int(b.strip().lstrip("v")) + 1))
    elif raw_regs:
        for r in raw_regs.split(","):
            r = r.strip()
            if r:
                regs.append(int(r.lstrip("v")))
    params = m.group(5)
    return {
        "mnemonic": mnemonic,
        "is_static": "static" in mnemonic,
        "regs": regs,
        "class": m.group(3),
        "name": m.group(4),
        "params": split_params(params),
        "proto": params + m.group(6),
        "ret": m.group(6),
    }


_ZERO_OPS = {
    "eqz": "eq",
    "nez": "ne",
    "ltz": "lt",
    "gez": "ge",
    "gtz": "gt",
    "lez": "le",
}
_FLIP = {"lt": "gt", "gt": "lt", "le": "ge", "ge": "le", "eq": "eq", "ne": "ne"}


def normalize_gate(op, reverse):
    if op in _ZERO_OPS:
        op = _ZERO_OPS[op]
    if reverse:
        op = _FLIP.get(op, op)
    return op


# ---------------------------------------------------------------------------
# register flow
# ---------------------------------------------------------------------------

UNKNOWN = ("unknown",)


def _value_of(regs, r):
    return regs.get(r, UNKNOWN)


def analyze_method(
    m, fmt, invoke_filter=None, collect_sputs=False, collect_gates=False, collect_fields=False
):
    """Run the register flow over one method.

    Returns a dict with ``invokes``, ``sputs``, ``gates`` and ``fields`` (each a
    list of dicts); ordered by program counter.  ``invokes`` keeps only callees
    for which ``invoke_filter(class_desc, name)`` is true.  ``fields`` records
    every ``sget`` when ``collect_fields`` is set.
    """
    regs = {}
    last_invoke = None
    invokes = []
    sputs = []
    gates = []
    fields = []

    for pc, text in m.instrs:
        space = text.find(" ")
        mnemonic = text if space < 0 else text[:space]

        # ---- invokes ----
        inv = parse_invoke(text)
        if inv is not None:
            argvals = [_value_of(regs, r) for r in inv["regs"]]
            cls, name = inv["class"], inv["name"]
            if cls == "Ljava/lang/StringBuilder;":
                if name == "<init>":
                    recv = inv["regs"][0]
                    if len(inv["regs"]) > 1:
                        regs[recv] = ("sb", [_value_of(regs, inv["regs"][1])])
                    else:
                        regs[recv] = ("sb", [])
                elif name == "append":
                    recv = inv["regs"][0]
                    cur = regs.get(recv)
                    parts = list(cur[1]) if (cur and cur[0] == "sb") else []
                    parts.append(_value_of(regs, inv["regs"][1]) if len(inv["regs"]) > 1 else UNKNOWN)
                    regs[recv] = ("sb", parts)
            if invoke_filter is None or invoke_filter(cls, name):
                invokes.append(
                    {
                        "pc": pc,
                        "mnemonic": inv["mnemonic"],
                        "class": cls,
                        "name": name,
                        "params": inv["params"],
                        "proto": inv["proto"],
                        "ret": inv["ret"],
                        "is_static": inv["is_static"],
                        "regs": inv["regs"],
                        "values": argvals,
                    }
                )
            last_invoke = inv
            continue

        # ---- move-result (must immediately follow an invoke) ----
        if mnemonic.startswith("move-result"):
            dst = parse_move(text)
            if dst is not None:
                d = dst[0]
                if last_invoke is not None and last_invoke["class"] == "Ljava/lang/StringBuilder;":
                    recv = last_invoke["regs"][0]
                    regs[d] = regs.get(recv, UNKNOWN)
                else:
                    regs[d] = UNKNOWN
            last_invoke = None
            continue

        last_invoke = None

        # ---- const-string ----
        cs = parse_const_string(text)
        if cs is not None:
            regs[cs[0]] = ("str", cs[1])
            continue

        # ---- const int / long ----
        ci = parse_const_int(text)
        if ci is not None:
            regs[ci[0]] = ("int", ci[1])
            continue

        # ---- move ----
        mv = parse_move(text)
        if mv is not None:
            dst, src = mv
            regs[dst] = regs.get(src, UNKNOWN) if src is not None else UNKNOWN
            continue

        # ---- sget ----
        sg = parse_sget(text)
        if sg is not None:
            r, c, n, t = sg
            regs[r] = ("field", c, n, t)
            if collect_fields:
                fields.append({"pc": pc, "class": c, "name": n, "type": t})
            continue

        # ---- new-instance ----
        ni = parse_new_instance(text)
        if ni is not None:
            r, c = ni
            regs[r] = ("sb", []) if c == "Ljava/lang/StringBuilder;" else ("obj", c)
            continue

        # ---- sput ----
        sp = parse_sput(text)
        if sp is not None:
            r, c, n, t = sp
            if collect_sputs:
                sputs.append(
                    {"pc": pc, "class": c, "name": n, "type": t, "value": _value_of(regs, r)}
                )
            continue

        # ---- if comparisons (SDK gates) ----
        if collect_gates and mnemonic.startswith("if-"):
            mif = _RE_IF.match(text)
            if mif:
                op = mif.group(1)
                r1 = int(mif.group(2))
                r2 = int(mif.group(3)) if mif.group(3) is not None else None
                v1 = _value_of(regs, r1)
                v2 = _value_of(regs, r2) if r2 is not None else ("int", 0)
                for a, b, rev in ((v1, v2, False), (v2, v1, True)):
                    if a and a[0] == "field" and a[1] == "Landroid/os/Build$VERSION;":
                        gates.append(
                            {
                                "pc": pc,
                                "field_class": a[1],
                                "field": a[2],
                                "op": normalize_gate(op, rev),
                                "value": b,
                            }
                        )

        # ---- conservative destination invalidation ----
        f = fmt.get(mnemonic)
        if f in DST_FORMATS:
            r = first_reg(text)
            if r is not None:
                regs.pop(r, None)

    return {"invokes": invokes, "sputs": sputs, "gates": gates, "fields": fields}


# ---------------------------------------------------------------------------
# executed-method sets
# ---------------------------------------------------------------------------

PHASE_FILES = (
    ("println", "methods_println.tsv"),
    ("launch", "methods_hello_views_launch.tsv"),
    ("interaction", "methods_hello_views_interaction.tsv"),
)


def load_executed(trace_dir):
    """Return ``phase -> set[(class, method, proto)]``."""
    sets = {}
    for phase, fname in PHASE_FILES:
        path = os.path.join(trace_dir, fname)
        s = set()
        with open(path, "r", errors="replace") as f:
            f.readline()  # header
            for line in f:
                p = line.rstrip("\n").split("\t")
                if len(p) < 6:
                    continue
                s.add((p[3], p[4], p[5]))
        sets[phase] = s
    return sets


def phases_of(cls_dotted, name, proto, executed):
    return [p for p, s in executed.items() if (cls_dotted, name, proto) in s]


PHASE_ORDER = ("println", "launch", "interaction")


class ExecutedIndex:
    """Fast membership + phase lookup over the three executed-method sets."""

    def __init__(self, sets):
        self.sets = sets
        self.union = set()
        for s in sets.values():
            self.union |= s

    def phase_of(self, cls_dotted, name, proto):
        return [p for p in PHASE_ORDER if (cls_dotted, name, proto) in self.sets.get(p, ())]


def resolve_value(value, static, depth=0):
    """Resolve a register value to a string, an ("sb", parts) concat, or None."""
    if value is None or depth > 8:
        return None
    kind = value[0]
    if kind == "str":
        return value[1]
    if kind == "int":
        return str(value[1])
    if kind == "field":
        field = static.get((value[1], value[2], value[3]))
        if field is None:
            for (c, n, _t), v in static.items():
                if c == value[1] and n == value[2]:
                    field = v
                    break
        return resolve_value(field, static, depth + 1)
    if kind == "sb":
        return ("sb", [resolve_value(p, static, depth + 1) for p in value[1]])
    return None


# ---------------------------------------------------------------------------
# DEX discovery
# ---------------------------------------------------------------------------

BOOTCLASSPATH_JARS = (
    "core-oj",
    "core-libart",
    "okhttp",
    "bouncycastle",
    "apache-xml",
    "framework",
    "framework-graphics",
    "ext",
    "telephony-common",
    "voip-common",
    "ims-common",
    "core-icu4j",
)


def boot_dex_paths(boot_dir):
    out = []
    for jar in BOOTCLASSPATH_JARS:
        d = os.path.join(boot_dir, "dex", jar)
        if not os.path.isdir(d):
            continue
        for f in sorted(os.listdir(d)):
            if f.endswith(".dex"):
                out.append(os.path.join(d, f))
    return out


def app_dex_paths(root):
    out = []
    base = os.path.join(root, "fixtures", "build", "apps")
    if not os.path.isdir(base):
        return out
    for app in sorted(os.listdir(base)):
        for rel in (os.path.join("dex", "classes.dex"), "classes.dex"):
            p = os.path.join(base, app, rel)
            if os.path.isfile(p):
                out.append(p)
                break
    return out


def default_dex_paths(root, boot_dir):
    return boot_dex_paths(boot_dir) + app_dex_paths(root)


def collect_static(dexdump_cmd, dex_paths, fmt):
    """Return ``(class, field, type) -> value`` for every ``<clinit>`` sput.

    Collected in a dedicated pass so that call-site resolution sees fields no
    matter where their declaring class sits in the DEX visit order.
    """
    static = {}
    for dex in dex_paths:
        for m in iter_methods(dexdump_cmd, dex):
            if m.name != "<clinit>":
                continue
            res = analyze_method(
                m, fmt, invoke_filter=lambda c, n: False, collect_sputs=True, collect_gates=False
            )
            for s in res["sputs"]:
                k = (s["class"], s["name"], s["type"])
                if k not in static or static[k][0] == "unknown":
                    static[k] = s["value"]
    return static


# ---------------------------------------------------------------------------
# system properties
# ---------------------------------------------------------------------------


def parse_props(path):
    props = OrderedDict()
    if not os.path.isfile(path):
        return props
    with open(path, "r", errors="replace") as f:
        for line in f:
            line = line.strip()
            if not line or line.startswith("#") or "=" not in line:
                continue
            k, v = line.split("=", 1)
            props[k.strip()] = v.strip()
    return props


# AOSP-derived "safe" defaults for keys the executed framework reads but that a
# device image may not carry in build.prop.  Values are chosen so the framework
# behaves like a normal user device; anything not listed is served as "".
KNOWN_DEFAULT_PROPS = OrderedDict(
    [
        ("ro.build.version.sdk", "34"),
        ("ro.build.version.release", "14"),
        ("ro.build.version.codename", "REL"),
        ("ro.build.version.all_codenames", "REL"),
        ("ro.build.version.preview_sdk", "0"),
        ("ro.build.version.min_supported_target_sdk", "28"),
        ("ro.build.version.security_patch", "2023-09-05"),
        ("ro.build.fingerprint", "google/sdk_gphone64_x86_64/emu64xa:14/UE1A.230829.050/12077443:userdebug/dev-keys"),
        ("ro.build.description", "sdk_gphone64_x86_64-userdebug 14 UE1A.230829.050 12077443 dev-keys"),
        ("ro.hw_timeout_multiplier", "1"),
        ("ro.build.type", "user"),
        ("ro.build.tags", "release-keys"),
        ("ro.build.user", "android-build"),
        ("ro.build.host", "android-build"),
        ("ro.build.flavor", "generic"),
        ("ro.product.model", "ACOS"),
        ("ro.product.name", "generic"),
        ("ro.product.device", "generic"),
        ("ro.product.board", "generic"),
        ("ro.product.manufacturer", "unknown"),
        ("ro.product.brand", "generic"),
        ("ro.product.locale", "en-US"),
        ("ro.product.cpu.abi", "x86_64"),
        ("ro.product.cpu.abilist", "x86_64,arm64-v8a"),
        ("ro.product.cpu.abilist32", ""),
        ("ro.product.cpu.abilist64", "x86_64,arm64-v8a"),
        ("ro.hardware", "goldfish"),
        ("ro.hardware.egl", "emulation"),
        ("ro.board.platform", "goldfish"),
        ("ro.kernel.qemu", "1"),
        ("ro.kernel.qemu.gles", "1"),
        ("ro.opengles.version", "196610"),
        ("ro.debuggable", "0"),
        ("ro.secure", "1"),
        ("ro.allow.mock.location", "0"),
        ("ro.treble.enabled", "true"),
        ("ro.boot.qemu.avd_name", ""),
        ("persist.sys.locale", "en-US"),
        ("persist.sys.timezone", "UTC"),
        ("sys.boot_completed", "1"),
        ("dev.bootcomplete", "1"),
        ("debug.atrace.tags.enableflags", "0"),
        ("debug.hwui.renderer", "skiagl"),
        ("debug.hwui.use_buffer_age", "1"),
        ("debug.hwui.trace_gpu_resources", "false"),
        ("debug.sf.nobootanimation", "0"),
        ("dalvik.vm.heapstartsize", "8m"),
        ("dalvik.vm.heapsize", "512m"),
        ("dalvik.vm.heapgrowthlimit", "256m"),
        ("dalvik.vm.heapminfree", "512k"),
        ("dalvik.vm.heapmaxfree", "8m"),
        ("dalvik.vm.heapidealfree", "4m"),
        ("dalvik.vm.heaptargetutilization", "0.75"),
        ("dalvik.vm.dex2oat-filter", "speed"),
        ("dalvik.vm.image-dex2oat-filter", "speed"),
        ("dalvik.vm.stack-trace-dir", "anr"),
        ("dalvik.vm.stack-trace-file", "/data/anr/traces.txt"),
    ]
)


def recommended_prop_value(key):
    """Recommended value plus a one-line rationale for a key with no image value."""
    if key in KNOWN_DEFAULT_PROPS:
        return KNOWN_DEFAULT_PROPS[key], "AOSP default (not in the extracted build.prop)"
    if key.startswith("ro."):
        return "", "read-only key absent from the image; empty string is the device behaviour for an unset property"
    if key.startswith("persist."):
        return "", "persistent key absent; empty string matches an unset property"
    if key.startswith("debug."):
        return "", "debug key absent; empty string matches an unset property"
    if key.startswith("sys."):
        return "", "system key absent; empty string matches an unset property"
    if key.startswith("init.svc."):
        return "running", "service-state key; ACOS has no init, 'running'/empty are both defensible"
    return "", "unset on the reference image; empty string is the safe device default"


# ---------------------------------------------------------------------------
# small helpers
# ---------------------------------------------------------------------------


def load_json(path):
    if not os.path.isfile(path):
        return None
    with open(path, "r", errors="replace") as f:
        return json.load(f)


def read_text(path):
    if not os.path.isfile(path):
        return ""
    with open(path, "r", errors="replace") as f:
        return f.read()


def write_tsv(path, header, rows):
    os.makedirs(os.path.dirname(path), exist_ok=True)
    with open(path, "w", encoding="utf-8") as f:
        f.write("\t".join(header) + "\n")
        for row in rows:
            f.write("\t".join("" if c is None else str(c) for c in row) + "\n")


def phase_str(phases):
    return ",".join(phases) if phases else "-"
