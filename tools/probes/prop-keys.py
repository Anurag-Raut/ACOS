#!/usr/bin/env python3
"""T-45: static system-property key census for the executed framework code.

For every method in the ART dynamic-trace executed-method sets (console
``println``, app ``launch`` and app ``interaction``) this tool decodes the
``const-string`` arguments passed to ``android.os.SystemProperties`` accessors.
It follows simple ``const-string`` / ``move`` / ``sget-object`` register flows
and resolves non-inlined ``static final String`` fields from the DEX ``<clinit>``
bodies.

Two supplementary sources are added so the property table is complete for ACOS
bring-up, and each row records its ``origin``:

* ``android.os.Build.<clinit>`` / ``android.os.Build$VERSION.<clinit>`` — the
  zygote-time reads that the boot image hides; the ``Build.*`` field each read
  initialises is reported in ``build_field``.
* ``android.sysprop.*`` accessors (e.g. ``DeviceProperties``) that are
  *reachable* from the executed code or from ``Build`` — the generated wrappers
  that forward a const-string to ``SystemProperties``.

Output ``research/ambient-state/sysprops.tsv`` gives the key, the caller
default, the accessor, the calling method(s), the phase(s) in which the call
site executes, and the value ACOS should serve (from the extracted Android 14
``build.prop`` or a documented AOSP default).  Keys with no image value are
emitted with ``status=gap``.

Run ``--selftest`` for the known-answer fixture (built with javac + d8 into
``.cache/probes/selftest``).
"""

from __future__ import annotations

import argparse
import os
import subprocess
import sys
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import acos_ambient as A  # noqa: E402

SYSPROP_CLASS = "Landroid/os/SystemProperties;"
BUILD_CLASS = "Landroid/os/Build;"
BUILD_VERSION_CLASS = "Landroid/os/Build$VERSION;"
ACCESSORS = {"get", "getInt", "getLong", "getBoolean", "find", "addChangeCallback"}
SUPPLEMENTARY_METHODS = {
    (BUILD_CLASS, "<clinit>"),
    (BUILD_VERSION_CLASS, "<clinit>"),
}
PHASE_ORDER = A.PHASE_ORDER
ExecutedIndex = A.ExecutedIndex
resolve_value = A.resolve_value


def classify(cls, name):
    """Return the accessor label when (cls, name) reads a system property."""
    if cls == SYSPROP_CLASS:
        if name in ACCESSORS or name.startswith("native_"):
            return "SystemProperties." + name
        return None
    if cls == BUILD_CLASS and name.startswith("-$$Nest$sm"):
        # javac nests Build.getString/getStringList helpers; they forward the
        # const-string to SystemProperties.get.  Recover the key at the caller.
        return "Build." + name[len("-$$Nest$sm") :]
    return None


def key_and_dynamic(value, static):
    resolved = resolve_value(value, static)
    if isinstance(resolved, str):
        return resolved, False
    if isinstance(resolved, tuple) and resolved and resolved[0] == "sb":
        known = [p for p in resolved[1] if isinstance(p, str)]
        return (known[0] if known else None), True
    return None, False


def _param_layout(iv):
    offset = 0 if iv["is_static"] else 1
    key_idx = None
    for i, p in enumerate(iv["params"]):
        if p == "Ljava/lang/String;":
            key_idx = i
            break
    return offset, key_idx


def _string_arg(iv):
    offset, key_idx = _param_layout(iv)
    if key_idx is None:
        return None
    idx = key_idx + offset
    return iv["values"][idx] if idx < len(iv["values"]) else None


def _default_arg(iv, static):
    if iv["name"] in ("addChangeCallback", "find"):
        return ""
    offset, key_idx = _param_layout(iv)
    if key_idx is None:
        return ""
    default_idx = key_idx + 1
    if default_idx >= len(iv["params"]):
        return ""
    idx = default_idx + offset
    if idx >= len(iv["values"]):
        return ""
    resolved = resolve_value(iv["values"][idx], static)
    if isinstance(resolved, str):
        return resolved
    if isinstance(resolved, tuple) and resolved and resolved[0] == "sb":
        parts = [p for p in resolved[1] if isinstance(p, str)]
        return parts[0] if parts else "(dynamic)"
    return ""


def scan(dexdump_cmd, dex_paths, executed, fmt, static, all_methods=False):
    """Return the call sites, resolved against the pre-collected static map."""
    calls = []
    sysprop_calls = []
    reachable = set()  # (class, name, proto) of generated sysprop accessors invoked from live code
    edges = {}  # sysprop method -> sysprop methods it invokes

    for dex in dex_paths:
        for m in A.iter_methods(dexdump_cmd, dex):
            cls = m.class_desc
            is_sysprop = cls.startswith("Landroid/sysprop/")
            supplementary = (cls, m.name) in SUPPLEMENTARY_METHODS
            key = (m.class_dotted, m.name, m.proto)
            phases = [] if all_methods else executed.phase_of(*key)
            is_exec = all_methods or bool(phases)
            interesting = is_exec or supplementary
            if not interesting and not is_sysprop:
                continue

            res = A.analyze_method(
                m,
                fmt,
                invoke_filter=lambda c, n: True,
                collect_sputs=interesting and m.name == "<clinit>",
                collect_gates=False,
            )

            method_key = (cls, m.name, m.proto)
            # reachability of generated sysprop wrappers
            if interesting:
                for iv in res["invokes"]:
                    if iv["class"].startswith("Landroid/sysprop/"):
                        reachable.add((iv["class"], iv["name"], iv["proto"]))
            if is_sysprop:
                for iv in res["invokes"]:
                    if iv["class"].startswith("Landroid/sysprop/"):
                        edges.setdefault(method_key, set()).add((iv["class"], iv["name"], iv["proto"]))

            target_calls = calls if interesting else sysprop_calls
            for iv in res["invokes"]:
                accessor = classify(iv["class"], iv["name"])
                if accessor is None:
                    continue
                key_val, dyn = key_and_dynamic(_string_arg(iv), static)
                build_field = ""
                if cls in (BUILD_CLASS, BUILD_VERSION_CLASS):
                    for s in res["sputs"]:
                        if s["pc"] > iv["pc"]:
                            build_field = s["name"]
                            break
                target_calls.append(
                    {
                        "key": key_val,
                        "dynamic": dyn,
                        "default": _default_arg(iv, static),
                        "accessor": accessor,
                        "caller": "%s->%s%s" % (m.class_dotted, m.name, m.proto),
                        "phases": phases,
                        "origin": "executed" if is_exec else "supplementary",
                        "build_field": build_field,
                        "method_key": method_key,
                    }
                )

    # transitive closure over the generated sysprop accessors
    changed = True
    while changed:
        changed = False
        for src, dsts in edges.items():
            if src in reachable:
                for d in dsts:
                    if d not in reachable:
                        reachable.add(d)
                        changed = True
    for c in sysprop_calls:
        if c["method_key"] in reachable:
            c = dict(c)
            c["origin"] = "sysprop"
            calls.append(c)
    return calls


def load_known_values(boot_dir):
    files = []
    props = A.parse_props(os.path.join(boot_dir, "build.prop"))
    if props:
        files.append("build.prop")
    for name in ("default.prop", "system/build.prop", "vendor/build.prop", "prop.default"):
        p = os.path.join(boot_dir, name)
        if os.path.isfile(p):
            files.append(name)
            for k, v in A.parse_props(p).items():
                props.setdefault(k, v)
    return props, files


def value_for_key(key, props):
    if key is None:
        return "-", "dynamic", "unknown key (prefix only)"
    if key in props:
        return props[key], "known", "extracted from the Android 14 image"
    val, why = A.recommended_prop_value(key)
    return val, "gap", why


def build_rows(calls, props):
    groups = {}
    for c in calls:
        if c["key"] is None and not c["dynamic"] and not c["accessor"].endswith(
            "addChangeCallback"
        ):
            continue  # a pass-through of a runtime parameter, not a key
        gk = (c["key"], c["dynamic"], c["accessor"])
        g = groups.setdefault(
            gk,
            {"defaults": set(), "callers": set(), "phases": set(), "origins": set(), "build_fields": set()},
        )
        g["defaults"].add(c["default"])
        g["callers"].add(c["caller"])
        g["phases"].update(c["phases"])
        g["origins"].add(c["origin"])
        if c["build_field"]:
            g["build_fields"].add(c["build_field"])

    rows = []
    for (key, dyn, accessor), g in groups.items():
        if accessor.endswith("addChangeCallback"):
            rows.append(
                [
                    "(callback)",
                    "",
                    accessor,
                    " | ".join(sorted(g["callers"])),
                    ",".join(p for p in PHASE_ORDER if p in g["phases"]) or "-",
                    "-",
                    "n/a",
                    "no",
                    ",".join(sorted(g["origins"])),
                    "",
                    "registers a change callback; no key argument",
                ]
            )
            continue
        if dyn:
            value, status, note = "-", "dynamic", "key assembled at runtime; serve the prefix namespace"
        else:
            value, status, note = value_for_key(key, props)
        defaults = sorted(d for d in g["defaults"] if d != "")
        rows.append(
            [
                key if key is not None else "(unknown)",
                "|".join(defaults),
                accessor,
                " | ".join(sorted(g["callers"])),
                ",".join(p for p in PHASE_ORDER if p in g["phases"]) or "-",
                value,
                status,
                "yes" if dyn else "no",
                ",".join(sorted(g["origins"])),
                " | ".join(sorted(g["build_fields"])),
                note,
            ]
        )
    rows.sort(key=lambda r: (r[6], r[0] or "", r[2]))
    return rows


HEADER = [
    "key",
    "default_arg",
    "accessor_kind",
    "calling_methods",
    "executed_in",
    "value_to_supply",
    "status",
    "dynamic",
    "origin",
    "build_field",
    "note",
]


def run(dexdump_cmd, dex_paths, executed, fmt, all_methods, boot_dir):
    static = A.collect_static(dexdump_cmd, dex_paths, fmt)
    calls = scan(dexdump_cmd, dex_paths, executed, fmt, static, all_methods)
    props, prop_files = load_known_values(boot_dir)
    rows = build_rows(calls, props)
    summary = {
        "call_sites": len(calls),
        "groups": len(rows),
        "known": sum(1 for r in rows if r[6] == "known"),
        "gap": sum(1 for r in rows if r[6] == "gap"),
        "dynamic": sum(1 for r in rows if r[6] == "dynamic"),
        "by_phase": {p: sum(1 for c in calls if p in c["phases"]) for p in PHASE_ORDER},
        "keys_by_phase": {
            p: len({c["key"] for c in calls if p in c["phases"] and c["key"] is not None})
            for p in PHASE_ORDER
        },
        "by_origin": {o: sum(1 for c in calls if c["origin"] == o) for o in ("executed", "supplementary", "sysprop")},
        "static_fields": len(static),
        "prop_files": prop_files,
        "dex_files": len(dex_paths),
    }
    return rows, summary


# ---------------------------------------------------------------------------
# selftest
# ---------------------------------------------------------------------------


def build_selftest_dex(root):
    sdk = os.environ.get("ANDROID_HOME", os.path.expanduser("~/Android/Sdk"))
    bt = os.environ.get("BUILD_TOOLS_VERSION", "35.0.0")
    android_jar = os.path.join(sdk, "platforms", "android-34", "android.jar")
    d8 = os.path.join(sdk, "build-tools", bt, "d8")
    if not os.path.isfile(android_jar):
        raise SystemExit("selftest: android.jar not found: %s" % android_jar)
    if not os.access(d8, os.X_OK):
        raise SystemExit("selftest: d8 not found: %s" % d8)
    src = os.path.join(root, "tools", "probes", "selftest")
    out = os.path.join(root, ".cache", "probes", "selftest")
    classes = os.path.join(out, "classes")
    subprocess.run(["rm", "-rf", classes], check=True)
    os.makedirs(classes, exist_ok=True)
    sources = [
        os.path.join(src, "AmbientSelftest.java"),
        os.path.join(src, "Keys.java"),
        os.path.join(src, "stub", "android", "os", "SystemProperties.java"),
    ]
    subprocess.run(["javac", "--release", "11", "-cp", android_jar, "-d", classes] + sources, check=True)
    class_files = []
    for dirpath, _d, files in os.walk(classes):
        for f in sorted(files):
            if f.endswith(".class"):
                class_files.append(os.path.join(dirpath, f))
    subprocess.run([d8, "--lib", android_jar, "--min-api", "26", "--output", out] + class_files, check=True)
    return os.path.join(out, "classes.dex")


def selftest(root, dexdump_cmd, fmt):
    dex = build_selftest_dex(root)
    rows, _summary = run(dexdump_cmd, [dex], ExecutedIndex({}), fmt, True, "/nonexistent")
    table = {}
    for r in rows:
        table.setdefault((r[0], r[2]), r)

    def check(key, accessor, default=None, dynamic=False, caller_sub=None):
        row = table.get((key, accessor))
        assert row is not None, "missing key %r accessor %r" % (key, accessor)
        if default is not None:
            assert row[1] == default, "default for %r: got %r want %r" % (key, row[1], default)
        assert row[7] == ("yes" if dynamic else "no"), "dynamic flag for %r" % key
        if caller_sub is not None:
            assert caller_sub in row[3], "caller for %r: %r" % (key, row[3])

    check("ro.selftest.key", "SystemProperties.get", default="fallback", caller_sub="propDefaults")
    check("debug.selftest.int", "SystemProperties.getInt", default="42", caller_sub="propInt")
    check("debug.selftest.static", "SystemProperties.get", caller_sub="propStaticField")
    check("ro.selftest.", "SystemProperties.get", dynamic=True, caller_sub="propDynamic")
    check("debug.selftest.long", "SystemProperties.getLong", default="7")
    check("debug.selftest.bool", "SystemProperties.getBoolean", default="1")
    print("prop-keys selftest: PASS (%d rows from %s)" % (len(rows), os.path.relpath(dex, root)))
    return 0


# ---------------------------------------------------------------------------
# CLI
# ---------------------------------------------------------------------------


def make_argparser():
    p = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    p.add_argument("--out", default=None)
    p.add_argument("--dex", action="append", default=None)
    p.add_argument("--boot", default=None)
    p.add_argument("--trace", default=None)
    p.add_argument("--all-methods", action="store_true")
    p.add_argument("--selftest", action="store_true")
    p.add_argument("--quiet", action="store_true")
    return p


def main(argv=None):
    args = make_argparser().parse_args(argv)
    root = A.repo_root()
    dirs = A.default_dirs(root)
    boot = args.boot or dirs["boot"]
    trace = args.trace or dirs["trace"]
    out = args.out or os.path.join(dirs["out"], "sysprops.tsv")
    fmt = A.load_format_map(root)
    dexdump_cmd = A.resolve_dexdump(root)

    if args.selftest:
        return selftest(root, dexdump_cmd, fmt)

    dex_paths = list(args.dex) if args.dex else A.default_dex_paths(root, boot)
    executed = ExecutedIndex(A.load_executed(trace))

    t0 = time.time()
    rows, summary = run(dexdump_cmd, dex_paths, executed, fmt, args.all_methods, boot)
    elapsed = time.time() - t0
    A.write_tsv(out, HEADER, rows)

    if not args.quiet:
        print(
            "prop-keys: %d call sites -> %d keys (%d known, %d gap, %d dynamic)"
            % (summary["call_sites"], summary["groups"], summary["known"], summary["gap"], summary["dynamic"])
        )
        print(
            "prop-keys: phases: %s (distinct keys: %s)"
            % (
                ", ".join("%s=%d" % (p, summary["by_phase"][p]) for p in PHASE_ORDER),
                ", ".join("%s=%d" % (p, summary["keys_by_phase"][p]) for p in PHASE_ORDER),
            )
        )
        print(
            "prop-keys: origin: %s" % ", ".join("%s=%d" % (o, n) for o, n in summary["by_origin"].items())
        )
        print(
            "prop-keys: scanned %d DEX, %d static String fields; props from %s"
            % (summary["dex_files"], summary["static_fields"], ", ".join(summary["prop_files"]) or "none")
        )
        print("prop-keys: wrote %s in %.2fs" % (os.path.relpath(out, root), elapsed))
    return 0


if __name__ == "__main__":
    sys.exit(main())
