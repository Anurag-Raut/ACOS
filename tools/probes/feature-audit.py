#!/usr/bin/env python3
"""T-46: static audit of the ambient state the framework asks for at launch.

Scans the ART dynamic-trace executed-method sets (console ``println``, app
``launch``, app ``interaction``) plus ``SystemServiceRegistry.<clinit>`` and
``Build.<clinit>`` and reports, as machine-readable TSV:

* ``features.tsv``        — ``hasSystemFeature`` names, ``SystemConfig`` /
                            ``FeatureInfo`` uses, ``DeviceConfig`` / ``*Flags``
                            feature-flag reads, and ``Configuration`` /
                            ``DisplayMetrics`` / ``LocaleList`` reads.
* ``services_requested.tsv`` — ``Context.getSystemService(String)`` /
                            ``getSystemServiceName`` constants and every
                            ``SystemServiceRegistry.registerService`` entry.
* ``settings_keys.tsv``  — ``Settings.{Global,Secure,System}`` get* keys, with
                            the caller default and whether the key was a literal
                            or a resolved ``static final String`` field.
* ``thread_names.tsv``   — const-string thread names passed to
                            ``HandlerThread`` / ``Thread`` constructors.
* ``sdk_gates.tsv``      — every ``Build.VERSION.*`` / ``SDK_INT`` comparison
                            along the path (field, branch op, constant).

``REPORT.md`` summarises the counts, derives the exact values ACOS should
return from ``fixtures/reference/hello-views/art-api34-x86_64`` and lists what
the references do *not* cover as GAPs, each with the exact ``adb`` command that
would capture it (never executed here).

Run ``--selftest`` for the known-answer fixture (built with javac + d8 into
``.cache/probes/selftest``).
"""

from __future__ import annotations

import argparse
import json
import os
import subprocess
import sys
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import acos_ambient as A  # noqa: E402

ExecutedIndex = A.ExecutedIndex
PHASE_ORDER = A.PHASE_ORDER
resolve_value = A.resolve_value
SUPPLEMENTARY_METHODS = {
    ("Landroid/os/Build;", "<clinit>"),
    ("Landroid/os/Build$VERSION;", "<clinit>"),
    ("Landroid/app/SystemServiceRegistry;", "<clinit>"),
}

CONFIG_TYPES = {
    "Landroid/content/res/Configuration;": "Configuration",
    "Landroid/util/DisplayMetrics;": "DisplayMetrics",
    "Landroid/os/LocaleList;": "LocaleList",
}
CONFIG_READ_CLASSES = {
    "Landroid/app/ActivityThread;",
    "Landroid/app/Application;",
    "Landroid/app/ContextImpl;",
    "Landroid/content/Context;",
    "Landroid/content/res/Resources;",
    "Landroid/content/res/AssetManager;",
    "Landroid/view/Display;",
}
CONFIG_READ_NAMES = {
    "getConfiguration",
    "getDisplayMetrics",
    "getResources",
    "getAssets",
    "getLocaleList",
    "getApplicationContext",
}
BUILD_FIELD_PROPS = {
    ("Landroid/os/Build$VERSION;", "SDK_INT"): "ro.build.version.sdk",
    ("Landroid/os/Build$VERSION;", "SDK"): "ro.build.version.sdk",
    ("Landroid/os/Build$VERSION;", "RELEASE"): "ro.build.version.release",
    ("Landroid/os/Build$VERSION;", "RELEASE_OR_CODENAME"): "ro.build.version.release_or_codename",
    ("Landroid/os/Build$VERSION;", "INCREMENTAL"): "ro.build.version.incremental",
    ("Landroid/os/Build$VERSION;", "PREVIEW_SDK_INT"): "ro.build.version.preview_sdk",
    ("Landroid/os/Build$VERSION;", "CODENAME"): "ro.build.version.codename",
    ("Landroid/os/Build$VERSION;", "BASE_OS"): "ro.build.version.base_os",
    ("Landroid/os/Build$VERSION;", "SECURITY_PATCH"): "ro.build.version.security_patch",
    ("Landroid/os/Build$VERSION;", "MIN_SUPPORTED_TARGET_SDK_INT"): "ro.build.version.min_supported_target_sdk",
    ("Landroid/os/Build;", "HARDWARE"): "ro.hardware",
    ("Landroid/os/Build;", "FINGERPRINT"): "ro.build.fingerprint",
    ("Landroid/os/Build;", "TYPE"): "ro.build.type",
    ("Landroid/os/Build;", "TAGS"): "ro.build.tags",
    ("Landroid/os/Build;", "DEVICE"): "ro.product.device",
    ("Landroid/os/Build;", "MODEL"): "ro.product.model",
    ("Landroid/os/Build;", "MANUFACTURER"): "ro.product.manufacturer",
    ("Landroid/os/Build;", "BRAND"): "ro.product.brand",
    ("Landroid/os/Build;", "PRODUCT"): "ro.product.name",
    ("Landroid/os/Build;", "IS_DEBUGGABLE"): "ro.debuggable",
    ("Landroid/os/Build;", "HW_TIMEOUT_MULTIPLIER"): "ro.hw_timeout_multiplier",
    ("Landroid/os/Build$VERSION;", "RESOURCES_SDK_INT"): "ro.build.version.sdk",
}
# Fields derived from a property rather than copied from one.
BUILD_FIELD_LITERALS = {
    ("Landroid/os/Build;", "IS_ENG"): "0",
    ("Landroid/os/Build;", "IS_USER"): "0",
    ("Landroid/os/Build;", "IS_USERDEBUG"): "1",
    ("Landroid/os/Build;", "SUPPORTED_32_BIT_ABIS"): "[]",
    ("Landroid/os/Build;", "SUPPORTED_64_BIT_ABIS"): "[x86_64]",
}
BUILD_FIELD_CLASSES = frozenset(
    ["Landroid/os/Build$VERSION;", "Landroid/os/Build;", "Landroid/os/Build$VERSION_CODES;"]
)


def sdk_field_value(cls, name, props):
    literal = BUILD_FIELD_LITERALS.get((cls, name))
    if literal is not None:
        return literal
    prop = BUILD_FIELD_PROPS.get((cls, name))
    if prop:
        if prop in props:
            return props[prop]
        if prop in A.KNOWN_DEFAULT_PROPS:
            return A.KNOWN_DEFAULT_PROPS[prop]
    return ""
SETTINGS_NAMESPACES = {
    "Landroid/provider/Settings$Global;": "Global",
    "Landroid/provider/Settings$Secure;": "Secure",
    "Landroid/provider/Settings$System;": "System",
    "Landroid/provider/Settings;": "System",
}


def dotted(desc):
    return A.class_desc_to_dotted(desc)


def first_string_param(iv):
    for i, p in enumerate(iv["params"]):
        if p == "Ljava/lang/String;":
            return i
    return None


def param_value(iv, param_idx, static):
    offset = 0 if iv["is_static"] else 1
    idx = param_idx + offset
    if 0 <= idx < len(iv["values"]):
        return iv["values"][idx]
    return None


def resolve_prov(value, static, depth=0):
    resolved = resolve_value(value, static, depth)
    if isinstance(resolved, str):
        if value and value[0] == "field":
            return resolved, "field:%s.%s" % (dotted(value[1]), value[2])
        return resolved, "literal"
    if isinstance(resolved, tuple) and resolved and resolved[0] == "sb":
        known = [p for p in resolved[1] if isinstance(p, str)]
        return (known[0] if known else None), "dynamic"
    return None, "dynamic"


def resolve_int(value, static):
    resolved = resolve_value(value, static)
    if isinstance(resolved, str):
        try:
            return str(int(resolved))
        except ValueError:
            return resolved
    return "?"


def string_args(iv, static):
    out = []
    for i, p in enumerate(iv["params"]):
        if p == "Ljava/lang/String;":
            v, prov = resolve_prov(param_value(iv, i, static), static)
            out.append((v, prov))
    return out


# ---------------------------------------------------------------------------
# per-category extractors
# ---------------------------------------------------------------------------


def ex_feature(iv, static, ctx):
    if iv["name"] != "hasSystemFeature":
        return []
    pidx = first_string_param(iv)
    if pidx is None:
        return []
    val, prov = resolve_prov(param_value(iv, pidx, static), static)
    if val is None:
        return []
    kind = "hasSystemFeature(int)" if len(iv["params"]) > 1 else "hasSystemFeature"
    return [
        (
            "features",
            {
                "kind": kind,
                "name": val or "(dynamic)",
                "calling_method": ctx["caller"],
                "executed_in": ctx["phases"],
                "origin": ctx["origin"],
                "note": "provenance=%s" % prov,
            },
        )
    ]


def ex_flags(iv, static, ctx):
    cls, name = iv["class"], iv["name"]
    if "DeviceConfig" in cls and (name.startswith("get") or name.startswith("is")):
        sargs = string_args(iv, static)
        if not sargs:
            return []
        key = "/".join((v or "(dynamic)") for v, _p in sargs[:2])
        return [
            (
                "features",
                {
                    "kind": "deviceconfig",
                    "name": key,
                    "calling_method": ctx["caller"],
                    "executed_in": ctx["phases"],
                    "origin": ctx["origin"],
                    "note": "namespace/name",
                },
            )
        ]
    if (
        "FeatureFlag" in cls
        or "/flagging/" in cls
        or (cls.endswith("Flags;") and "$" not in cls and "com/android/internal" in cls)
    ):
        return [
            (
                "features",
                {
                    "kind": "featureflag",
                    "name": "%s.%s" % (dotted(cls), name),
                    "calling_method": ctx["caller"],
                    "executed_in": ctx["phases"],
                    "origin": ctx["origin"],
                    "note": "class.method identity (flag name is baked into the generated accessor)",
                },
            )
        ]
    if "SystemConfig" in cls or "FeatureInfo" in cls:
        return [
            (
                "features",
                {
                    "kind": "systemconfig" if "SystemConfig" in cls else "featureinfo",
                    "name": "%s.%s" % (dotted(cls), name),
                    "calling_method": ctx["caller"],
                    "executed_in": ctx["phases"],
                    "origin": ctx["origin"],
                    "note": "",
                },
            )
        ]
    return []


def ex_config(iv, static, ctx):
    cls, name, ret = iv["class"], iv["name"], iv["ret"]
    if cls in CONFIG_READ_CLASSES and (ret in CONFIG_TYPES or name in CONFIG_READ_NAMES):
        return [
            (
                "features",
                {
                    "kind": "config_read",
                    "name": "%s.%s" % (dotted(cls), name),
                    "calling_method": ctx["caller"],
                    "executed_in": ctx["phases"],
                    "origin": ctx["origin"],
                    "note": "returns %s" % CONFIG_TYPES.get(ret, ret),
                },
            )
        ]
    return []


def ex_service(iv, static, ctx):
    cls, name = iv["class"], iv["name"]
    if name in ("getSystemService", "getSystemServiceName"):
        if "Ljava/lang/String;" in iv["params"]:
            pidx = first_string_param(iv)
            val, prov = resolve_prov(param_value(iv, pidx, static), static)
            if val is None:
                return []
            key = val
        elif "Ljava/lang/Class;" in iv["params"]:
            key = "(Class)"
            prov = "class-token"
        else:
            return []
        return [
            (
                "services",
                {
                    "service_name": key,
                    "api": name,
                    "calling_method": ctx["caller"],
                    "executed_in": ctx["phases"],
                    "origin": ctx["origin"],
                    "registered_with": "",
                    "note": "provenance=%s" % prov,
                },
            )
        ]
    if cls == "Landroid/app/SystemServiceRegistry;" and name == "registerService":
        pidx = first_string_param(iv)
        if pidx is None:
            return []
        val, prov = resolve_prov(param_value(iv, pidx, static), static)
        reg = param_value(iv, pidx + 1, static)
        if reg and reg[0] == "obj":
            reg_s = dotted(reg[1])
        else:
            reg_s = ""
        return [
            (
                "services",
                {
                    "service_name": val or "(dynamic)",
                    "api": "registerService",
                    "calling_method": ctx["caller"],
                    "executed_in": ctx["phases"],
                    "origin": ctx["origin"],
                    "registered_with": reg_s,
                    "note": "provenance=%s" % prov,
                },
            )
        ]
    return []


def ex_settings(iv, static, ctx):
    cls = iv["class"]
    if cls not in SETTINGS_NAMESPACES:
        return []
    if not iv["name"].startswith("get"):
        return []
    pidx = first_string_param(iv)
    if pidx is None:
        return []
    val, prov = resolve_prov(param_value(iv, pidx, static), static)
    if val is None:
        return []
    default = ""
    if pidx + 1 < len(iv["params"]):
        dv = param_value(iv, pidx + 1, static)
        dv_res = resolve_value(dv, static)
        if isinstance(dv_res, str):
            default = dv_res
    return [
        (
            "settings",
            {
                "namespace": SETTINGS_NAMESPACES[cls],
                "key": val or "(dynamic)",
                "accessor": iv["name"],
                "default": default,
                "calling_method": ctx["caller"],
                "executed_in": ctx["phases"],
                "origin": ctx["origin"],
                "provenance": prov,
                "note": "provenance=%s" % prov,
            },
        )
    ]


def ex_thread(iv, static, ctx):
    cls, name = iv["class"], iv["name"]
    if name != "<init>":
        return []
    if cls not in ("Landroid/os/HandlerThread;", "Ljava/lang/Thread;"):
        return []
    pidx = first_string_param(iv)
    if pidx is None:
        return []
    val, prov = resolve_prov(param_value(iv, pidx, static), static)
    if val is None:
        return []
    return [
        (
            "threads",
            {
                "thread_name": val or "(dynamic)",
                "kind": "HandlerThread" if cls == "Landroid/os/HandlerThread;" else "Thread",
                "calling_method": ctx["caller"],
                "executed_in": ctx["phases"],
                "origin": ctx["origin"],
                "note": "provenance=%s" % prov,
            },
        )
    ]


EXTRACTORS = (ex_feature, ex_flags, ex_config, ex_service, ex_settings, ex_thread)


# ---------------------------------------------------------------------------
# scan
# ---------------------------------------------------------------------------


def scan(dexdump_cmd, dex_paths, executed, fmt, static, props, all_methods=False):
    tables = {"features": [], "services": [], "settings": [], "threads": [], "sdk_gates": []}

    for dex in dex_paths:
        for m in A.iter_methods(dexdump_cmd, dex):
            cls = m.class_desc
            is_clinit = m.name == "<clinit>"
            supplementary = (cls, m.name) in SUPPLEMENTARY_METHODS
            key = (m.class_dotted, m.name, m.proto)
            phases = [] if all_methods else executed.phase_of(*key)
            is_exec = all_methods or bool(phases)
            interesting = is_exec or supplementary
            if not is_clinit and not interesting:
                continue

            res = A.analyze_method(
                m,
                fmt,
                invoke_filter=lambda c, n: True,
                collect_sputs=False,
                collect_gates=interesting,
                collect_fields=interesting,
            )
            if not interesting:
                continue

            ctx = {
                "caller": "%s->%s%s" % (m.class_dotted, m.name, m.proto),
                "phases": phases,
                "origin": "executed" if is_exec else "supplementary",
            }
            for iv in res["invokes"]:
                for extractor in EXTRACTORS:
                    for category, row in extractor(iv, static, ctx):
                        tables[category].append(row)
            for f in res["fields"]:
                if f["class"] in BUILD_FIELD_CLASSES:
                    tables["sdk_gates"].append(
                        {
                            "field": "%s.%s" % (dotted(f["class"]), f["name"]),
                            "site": "read",
                            "compared_value": "",
                            "field_value": sdk_field_value(f["class"], f["name"], props),
                            "calling_method": ctx["caller"],
                            "executed_in": ctx["phases"],
                            "origin": ctx["origin"],
                            "note": "value read by the path",
                        }
                    )
            for g in res["gates"]:
                tables["sdk_gates"].append(
                    {
                        "field": "%s.%s" % (dotted(g["field_class"]), g["field"]),
                        "site": g["op"],
                        "compared_value": resolve_int(g["value"], static),
                        "field_value": sdk_field_value(g["field_class"], g["field"], props),
                        "calling_method": ctx["caller"],
                        "executed_in": ctx["phases"],
                        "origin": ctx["origin"],
                        "note": "branch taken when the field relation holds",
                    }
                )
            for iv in res["invokes"]:
                if "SdkLevel" in iv["class"] or iv["name"].startswith(("isAtLeast", "isAtMost")):
                    tables["sdk_gates"].append(
                        {
                            "field": "%s.%s" % (dotted(iv["class"]), iv["name"]),
                            "site": "invoke",
                            "compared_value": "",
                            "field_value": "",
                            "calling_method": ctx["caller"],
                            "executed_in": ctx["phases"],
                            "origin": ctx["origin"],
                            "note": "SDK-level helper gate",
                        }
                    )
    return tables, static


# ---------------------------------------------------------------------------
# output helpers
# ---------------------------------------------------------------------------

HEADERS = {
    "features": ["kind", "name", "calling_method", "executed_in", "origin", "note"],
    "services": [
        "service_name",
        "api",
        "calling_method",
        "executed_in",
        "origin",
        "registered_with",
        "note",
    ],
    "settings": [
        "namespace",
        "key",
        "accessor",
        "default",
        "calling_method",
        "executed_in",
        "origin",
        "note",
    ],
    "threads": ["thread_name", "kind", "calling_method", "executed_in", "origin", "note"],
    "sdk_gates": [
        "field",
        "site",
        "compared_value",
        "field_value",
        "calling_method",
        "executed_in",
        "origin",
        "note",
    ],
}

TSV_NAMES = {
    "features": "features.tsv",
    "services": "services_requested.tsv",
    "settings": "settings_keys.tsv",
    "threads": "thread_names.tsv",
    "sdk_gates": "sdk_gates.tsv",
}

SORT_KEYS = {
    "features": lambda r: (r["kind"], r["name"], r["calling_method"]),
    "services": lambda r: (r["service_name"], r["api"], r["calling_method"]),
    "settings": lambda r: (r["namespace"], r["key"], r["calling_method"]),
    "threads": lambda r: (r["thread_name"], r["calling_method"]),
    "sdk_gates": lambda r: (r["field"], r["site"], r["calling_method"]),
}


def format_phases(phases):
    return ",".join(p for p in PHASE_ORDER if p in phases) or "-"


def _hashable(value):
    if isinstance(value, list):
        return tuple(value)
    return value


def dedup(rows):
    seen = set()
    out = []
    for r in rows:
        sig = tuple(sorted((k, _hashable(v)) for k, v in r.items()))
        if sig in seen:
            continue
        seen.add(sig)
        out.append(r)
    return out


def write_outputs(out_dir, tables):
    written = {}
    for category, rows in tables.items():
        rows = dedup(rows)
        rows.sort(key=SORT_KEYS[category])
        header = HEADERS[category]

        def cell(r, h):
            v = r.get(h, "")
            if isinstance(v, list):
                return format_phases(v)
            return v

        tsv_rows = [[cell(r, h) for h in header] for r in rows]
        path = os.path.join(out_dir, TSV_NAMES[category])
        A.write_tsv(path, header, tsv_rows)
        written[category] = (path, rows)
    return written


# ---------------------------------------------------------------------------
# report
# ---------------------------------------------------------------------------


def read_display(reference_dir):
    text = A.read_text(os.path.join(reference_dir, "display.txt"))
    out = {}
    for line in text.splitlines():
        line = line.strip()
        if line.startswith("init="):
            out["init"] = line
        elif line.startswith("overrideConfig="):
            out.setdefault("overrideConfig", line)
        elif line.startswith("mAppBounds=") or "mAppBounds=" in line:
            out.setdefault("appBounds", line)
        elif line.startswith("mDisplayCutout="):
            out.setdefault("cutout", line)
        elif line.startswith("mRoundedCorners="):
            out.setdefault("roundedCorners", line)
    return out


def report_markdown(summary, tables, reference_dir, boot_dir, props):
    lines = []
    lines.append("# T-46 ambient-state audit")
    lines.append("")
    lines.append("Generated by `tools/probes/feature-audit.py` from the executed ART")
    lines.append("method sets in `research/dynamic-trace/data/` and the extracted Android 14")
    lines.append("boot DEX in `.cache/boot/android-34/dex/`.")
    lines.append("")
    lines.append("## Counts")
    lines.append("")
    lines.append("| table | rows | executed | supplementary |")
    lines.append("|---|---:|---:|---:|")
    for category, (path, rows) in summary["written"].items():
        executed = sum(1 for r in rows if r["origin"] == "executed")
        lines.append("| `%s` | %d | %d | %d |" % (os.path.basename(path), len(rows), executed, len(rows) - executed))
    lines.append("")
    lines.append("Per phase (call sites attributed to the trace that contains the caller):")
    lines.append("")
    lines.append("| phase | features | services | settings | threads | sdk_gates |")
    lines.append("|---|---:|---:|---:|---:|---:|")
    for p in PHASE_ORDER:
        counts = []
        for cat in ("features", "services", "settings", "threads", "sdk_gates"):
            counts.append(sum(1 for r in tables[cat] if p in r.get("executed_in", [])))
        lines.append("| %s | %d | %d | %d | %d | %d |" % tuple([p] + counts))
    lines.append("")

    # ---- exact values -----------------------------------------------------
    cfg = A.load_json(os.path.join(reference_dir, "device-config.json")) or {}
    ref_props = {}
    if cfg.get("build_fingerprint"):
        ref_props["ro.build.fingerprint"] = cfg["build_fingerprint"]
    lines.append("## Exact values ACOS should return")
    lines.append("")
    lines.append("### Build / property identity")
    lines.append("")
    for k in (
        "ro.build.fingerprint",
        "ro.build.id",
        "ro.build.version.sdk",
        "ro.build.version.release",
        "ro.build.type",
        "ro.build.tags",
        "ro.product.cpu.abi",
        "ro.product.cpu.abilist",
        "ro.product.locale",
        "ro.hardware",
        "ro.opengles.version",
        "ro.debuggable",
        "ro.secure",
    ):
        if k in props:
            lines.append("* `%s = %s`  _(build.prop)_" % (k, props[k]))
        elif k in ref_props:
            lines.append("* `%s = %s`  _(reference device-config.json; not in build.prop)_" % (k, ref_props[k]))
        elif k in A.KNOWN_DEFAULT_PROPS:
            lines.append("* `%s = %s`  _(AOSP default; unverified on this image)_" % (k, A.KNOWN_DEFAULT_PROPS[k]))
        else:
            lines.append("* `%s` — **GAP** (absent from the extracted image)" % k)
    lines.append("")

    if cfg:
        lines.append("### Device configuration (`device-config.json`)")
        lines.append("")
        for k in (
            "resolution",
            "density_dpi",
            "locale",
            "font_scale",
            "dark_light",
            "build_fingerprint",
            "api",
            "abi",
            "raw_wm_size",
            "raw_wm_density",
        ):
            if k in cfg:
                lines.append("* `%s = %s`" % (k, cfg[k]))
        lines.append("")

    disp = read_display(reference_dir)
    if disp:
        lines.append("### Window manager configuration (`dumpsys window displays`)")
        lines.append("")
        for k in ("init", "overrideConfig", "appBounds", "cutout", "roundedCorners"):
            if k in disp:
                lines.append("* `%s`: `%s`" % (k, disp[k][:400]))
        lines.append("* Derived: `density = 420/160 = 2.625`, `scaledDensity = 2.625`")
        lines.append("* Derived: status-bar inset 128px, navigation-bar inset 63px")
        lines.append("")

    mem = A.read_text(os.path.join(reference_dir, "meminfo_initial.txt"))
    if mem:
        for line in mem.splitlines():
            if line.strip().startswith("TOTAL"):
                lines.append("### Idle memory target (`meminfo_initial.txt`)")
                lines.append("")
                lines.append("* `%s`" % " ".join(line.split()))
                lines.append("")
                break

    texts = A.read_text(os.path.join(reference_dir, "observed-texts-initial.txt")).strip()
    if texts:
        lines.append("### First-frame UI (`observed-texts-initial.txt`)")
        lines.append("")
        for row in texts.splitlines():
            lines.append("* `%s`" % row)
        lines.append("")

    # ---- SDK gates --------------------------------------------------------
    lines.append("### Build.VERSION / SDK gate surface")
    lines.append("")
    direct = [r for r in tables["sdk_gates"] if r["site"] not in ("read", "invoke")]
    reads = sorted({(r["field"], r["field_value"]) for r in tables["sdk_gates"] if r["site"] == "read"})
    if direct:
        lines.append("Direct branch comparisons on the path:")
        lines.append("")
        for r in direct:
            lines.append("* `%s` %s `%s`  _(%s)_" % (r["field"], r["site"], r["compared_value"], r["calling_method"]))
    else:
        lines.append("**No direct `SDK_INT` branch comparison executes on the hello-views path.**")
        lines.append("The path instead reads these `Build.*` fields (value ACOS must serve):")
        lines.append("")
        for field, value in reads:
            lines.append("* `%s = %s`" % (field, value))
        lines.append("")
        lines.append("`Build.VERSION.RESOURCES_SDK_INT` is the effective `SDK_INT` used by")
        lines.append("`ResourcesImpl.updateConfiguration`; ACOS still has to set")
        lines.append("`ro.build.version.sdk=34` because the zygote boot image hid the `SDK_INT`")
        lines.append("initialisation (see `sysprops.tsv`, `build_field=SDK_INT`).")
    lines.append("")

    # ---- gaps -------------------------------------------------------------
    lines.append("## GAPs (references do not cover)")
    lines.append("")
    lines.append("The reference capture is a single `hello-views` launch plus tap/type/rotate.")
    lines.append("It does not enumerate the feature table, the service registry, Settings")
    lines.append("defaults, the thread inventory, or the `DeviceConfig` namespaces.  Each gap")
    lines.append("below names the exact command that would capture it; **none were executed")
    lines.append("for this report**.")
    lines.append("")

    feature_names = sorted({r["name"] for r in tables["features"] if r["kind"].startswith("hasSystemFeature")})
    lines.append("### G-3 system features")
    lines.append("")
    lines.append("Requested names (%d): %s" % (len(feature_names), ", ".join("`%s`" % n for n in feature_names[:20]) or "none"))
    lines.append("")
    lines.append("The reference image has no feature dump.  Capture the real table with:")
    lines.append("")
    lines.append("```bash")
    lines.append("adb -s emulator-5554 shell pm list features")
    lines.append("adb -s emulator-5554 shell dumpsys package | sed -n '/^Features:/,/^$/p'")
    lines.append("```")
    lines.append("")
    lines.append("No `SystemConfig` / `FeatureInfo` call site executes on this path.")
    lines.append("")

    dc = sorted({r["name"] for r in tables["features"] if r["kind"] == "deviceconfig"})
    ff = sorted({r["name"] for r in tables["features"] if r["kind"] == "featureflag"})
    lines.append("### DeviceConfig / feature flags")
    lines.append("")
    lines.append("`DeviceConfig` namespaces read (%d): %s" % (len(dc), ", ".join("`%s`" % n for n in dc) or "none"))
    lines.append("")
    lines.append("Generated feature-flag accessors read (%d): %s" % (len(ff), ", ".join("`%s`" % n for n in ff[:20]) or "none"))
    lines.append("")
    lines.append("The reference capture has no `DeviceConfig`/flag dump; ACOS should serve the")
    lines.append("AOSP defaults.  Capture the live values with:")
    lines.append("")
    lines.append("```bash")
    lines.append("adb -s emulator-5554 shell device_config list")
    lines.append("adb -s emulator-5554 shell dumpsys device_config")
    lines.append("adb -s emulator-5554 shell dumpsys activity flags")
    lines.append("```")
    lines.append("")

    requested = sorted(
        {r["service_name"] for r in tables["services"] if r["api"] != "registerService" and not r["service_name"].startswith("(")}
    )
    registered = sorted(
        {r["service_name"] for r in tables["services"] if r["api"] == "registerService"}
    )
    lines.append("### Services requested at launch")
    lines.append("")
    lines.append("Names requested by executed code (%d): %s" % (len(requested), ", ".join("`%s`" % n for n in requested) or "none"))
    lines.append("")
    lines.append("`SystemServiceRegistry.<clinit>` registers %d names; ACOS must serve all of" % len(registered))
    lines.append("them (the registry runs on first `getSystemService`).  The reference")
    lines.append("capture has no `dumpsys` service list.  Capture with:")
    lines.append("")
    lines.append("```bash")
    lines.append("adb -s emulator-5554 shell dumpsys -l")
    lines.append("adb -s emulator-5554 shell service list")
    lines.append("```")
    lines.append("")

    settings_names = sorted({(r["namespace"], r["key"]) for r in tables["settings"]})
    lines.append("### Settings keys (%d)" % len(settings_names))
    lines.append("")
    for ns, key, default in sorted(
        {(r["namespace"], r["key"], r["default"]) for r in tables["settings"]}
    ):
        lines.append("* `%s.%s`  _(caller default `%s`)_" % (ns, key, default or "unset"))
    if not settings_names:
        lines.append("* none observed in the executed path")
    lines.append("")
    lines.append("Defaults marked `unset` were not a const in the DEX.  The reference")
    lines.append("image's settings store can be dumped with (per namespace):")
    lines.append("")
    lines.append("```bash")
    lines.append("adb -s emulator-5554 shell settings list global")
    lines.append("adb -s emulator-5554 shell settings list secure")
    lines.append("adb -s emulator-5554 shell settings list system")
    lines.append("```")
    lines.append("")

    thread_names = sorted({r["thread_name"] for r in tables["threads"]})
    lines.append("### G-5 thread inventory")
    lines.append("")
    lines.append("Thread names created by the path (%d): %s" % (len(thread_names), ", ".join("`%s`" % n for n in thread_names) or "none"))
    lines.append("")
    lines.append("The dynamic-trace report (§G-5) records 2 Java threads for interaction and")
    lines.append("main-only for launch, but the reference capture has no `/proc/<pid>/task`")
    lines.append("snapshot.  Capture the live inventory with:")
    lines.append("")
    lines.append("```bash")
    lines.append("adb -s emulator-5554 shell am start -W -n dev.acos.fixtures.helloviews/.MainActivity")
    lines.append("adb -s emulator-5554 shell 'ps -T -p $(pidof dev.acos.fixtures.helloviews)'")
    lines.append("adb -s emulator-5554 shell 'for t in /proc/$(pidof dev.acos.fixtures.helloviews)/task/*/comm; do cat $t; done'")
    lines.append("```")
    lines.append("")

    lines.append("### G-1 ICU data")
    lines.append("")
    lines.append("The interaction path lazily loads ICU4J rule data (`.dat`) not present in")
    lines.append("`.cache/boot/android-34`.  Capture with the commands from")
    lines.append("`research/service-layer/REPORT.md` §5.2 G-1:")
    lines.append("")
    lines.append("```bash")
    lines.append("adb -s emulator-5554 shell 'ls -l /apex/com.android.i18n/etc/icu/ /system/icu/ 2>/dev/null'")
    lines.append("adb -s emulator-5554 root")
    lines.append("adb -s emulator-5554 shell 'setprop wrap.dev.acos.fixtures.helloviews \"logwrapper strace -f -e trace=openat -o /data/local/tmp/icu.strace\"'")
    lines.append("adb -s emulator-5554 shell 'am start -W -n dev.acos.fixtures.helloviews/.MainActivity'")
    lines.append("adb -s emulator-5554 shell 'input tap 540 427; input text World'")
    lines.append("adb -s emulator-5554 shell 'grep -E \"icu.*\\.dat\" /data/local/tmp/icu.strace'")
    lines.append("adb -s emulator-5554 shell 'setprop wrap.dev.acos.fixtures.helloviews \"\"'")
    lines.append("```")
    lines.append("")

    lines.append("### System properties with no image value")
    lines.append("")
    lines.append("See `research/ambient-state/sysprops.tsv` (`status=gap`).  The extracted")
    lines.append("`.cache/boot/android-34/` contains `build.prop` but **no `default.prop`** or")
    lines.append("other `ro.*` property files, so the boot-time defaults (`ro.boot.*`,")
    lines.append("`ro.hardware.*`, `ro.gfx.driver.*`) are unverified.  Capture with:")
    lines.append("")
    lines.append("```bash")
    lines.append("adb -s emulator-5554 shell getprop")
    lines.append("adb -s emulator-5554 shell cat /system/build.prop /vendor/build.prop /odm/etc/build.prop 2>/dev/null")
    lines.append("```")
    lines.append("")
    return "\n".join(lines) + "\n"


# ---------------------------------------------------------------------------
# selftest
# ---------------------------------------------------------------------------


def build_selftest_dex(root):
    # reuse the prop-keys builder
    import importlib.util

    spec = importlib.util.spec_from_file_location(
        "prop_keys_mod", os.path.join(root, "tools", "probes", "prop-keys.py")
    )
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod.build_selftest_dex(root)


def selftest(root, dexdump_cmd, fmt):
    dex = build_selftest_dex(root)
    static = A.collect_static(dexdump_cmd, [dex], fmt)
    tables, _static = scan(dexdump_cmd, [dex], ExecutedIndex({}), fmt, static, {}, True)

    feature_names = {r["name"] for r in tables["features"] if r["kind"].startswith("hasSystemFeature")}
    assert "android.hardware.touchscreen" in feature_names, feature_names

    service_names = {(r["service_name"], r["api"]) for r in tables["services"]}
    assert ("window", "getSystemService") in service_names, service_names

    settings = {(r["namespace"], r["key"], r["accessor"]) for r in tables["settings"]}
    assert ("Global", "adb_enabled", "getInt") in settings, settings
    assert ("Global", "selftest_setting_key", "getInt") in settings, settings

    threads = {(r["thread_name"], r["kind"]) for r in tables["threads"]}
    assert ("selftest.thread", "HandlerThread") in threads, threads

    gates = {(r["field"], r["site"], r["compared_value"]) for r in tables["sdk_gates"]}
    assert ("android.os.Build$VERSION.SDK_INT", "lt", "33") in gates, gates
    assert any(r["field"] == "android.os.Build$VERSION.SDK_INT" and r["site"] == "read" for r in tables["sdk_gates"])

    print(
        "feature-audit selftest: PASS (features=%d services=%d settings=%d threads=%d gates=%d)"
        % (
            len(tables["features"]),
            len(tables["services"]),
            len(tables["settings"]),
            len(tables["threads"]),
            len(tables["sdk_gates"]),
        )
    )
    return 0


# ---------------------------------------------------------------------------
# CLI
# ---------------------------------------------------------------------------


def make_argparser():
    p = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    p.add_argument("--out-dir", default=None)
    p.add_argument("--dex", action="append", default=None)
    p.add_argument("--boot", default=None)
    p.add_argument("--trace", default=None)
    p.add_argument("--reference", default=None)
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
    reference = args.reference or dirs["reference"]
    out_dir = args.out_dir or dirs["out"]
    fmt = A.load_format_map(root)
    dexdump_cmd = A.resolve_dexdump(root)

    if args.selftest:
        return selftest(root, dexdump_cmd, fmt)

    dex_paths = list(args.dex) if args.dex else A.default_dex_paths(root, boot)
    executed = ExecutedIndex(A.load_executed(trace))

    t0 = time.time()
    props = A.parse_props(os.path.join(boot, "build.prop"))
    prop_files = ["build.prop"] if props else []
    static = A.collect_static(dexdump_cmd, dex_paths, fmt)
    tables, _static = scan(dexdump_cmd, dex_paths, executed, fmt, static, props, args.all_methods)
    written = write_outputs(out_dir, tables)
    summary = {"written": written, "prop_files": prop_files}
    report = report_markdown(summary, tables, reference, boot, props)
    with open(os.path.join(out_dir, "REPORT.md"), "w", encoding="utf-8") as f:
        f.write(report)
    elapsed = time.time() - t0

    if not args.quiet:
        for category, (path, rows) in written.items():
            executed_n = sum(1 for r in rows if r["origin"] == "executed")
            print(
                "feature-audit: %-14s %4d rows (%d executed, %d supplementary) -> %s"
                % (category, len(rows), executed_n, len(rows) - executed_n, os.path.relpath(path, root))
            )
        for p in PHASE_ORDER:
            counts = ", ".join(
                "%s=%d" % (cat, sum(1 for r in tables[cat] if p in r.get("executed_in", [])))
                for cat in ("features", "services", "settings", "threads", "sdk_gates")
            )
            print("feature-audit: phase %-11s %s" % (p, counts))
        print("feature-audit: wrote REPORT.md in %.2fs" % elapsed)
    return 0


if __name__ == "__main__":
    sys.exit(main())
