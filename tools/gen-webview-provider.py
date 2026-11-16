#!/usr/bin/env python3
"""Generate the hidden-API stubs and the WebView provider's method surface.

The framework's WebView provider is compiled against classes that are **not** in
the SDK `android.jar` (`WebViewFactoryProvider`, `WebViewProvider`,
`WebViewDelegate`, `WebView$PrivateAccess`, ...), so it cannot be compiled
against the public SDK alone. Everything here is generated from the method list
the pinned API-34 image actually calls (`research/spike-wv/provider/METHODS.md`,
extracted from the image's own dex), so the contract is measured, not
transcribed.

Two artefacts:

1. `hidden-stubs/` — compile-time-only stubs with the image's exact erased
   signatures. Never dexed into the provider APK: the image's boot classpath
   supplies the real classes at runtime.
2. `src/` — the provider classes, with **every** method of the four framework
   interfaces implemented. The implemented ones delegate to the provider's real
   code; the rest throw a structured `UnsupportedOperationException` naming the
   method and its dex descriptor (ADR-0031 and the repo's 0-false-passes rule:
   `unsupported` is structured, never a silent null).

Regenerate: `python3 tools/gen-webview-provider.py` (use `--check` to verify the
checked-in sources are current).
"""

from __future__ import annotations

import os
import sys

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
METHODS = os.path.join(ROOT, "research/spike-wv/provider/METHODS.md")
APP = os.path.join(ROOT, "fixtures/apps/webview-provider")
STUBS = os.path.join(APP, "hidden-stubs")
SRC = os.path.join(APP, "src")
PKG = "com.android.webview.chromium"
PKG_DIR = PKG.replace(".", "/")

PRIMITIVE = {
    "V": "void",
    "Z": "boolean",
    "B": "byte",
    "C": "char",
    "S": "short",
    "I": "int",
    "J": "long",
    "F": "float",
    "D": "double",
}
DEFAULT = {
    "void": "",
    "boolean": "false",
    "byte": "0",
    "char": "0",
    "short": "0",
    "int": "0",
    "long": "0L",
    "float": "0f",
    "double": "0d",
}

# Types the provider is compiled against that android.jar does not have.
# `WebView` is stubbed only because its `PrivateAccess` is hidden and javac
# resolves member types through the owner class.
STUB_CLASSES = {
    "android/webkit/WebViewFactoryProvider": "interface",
    "android/webkit/WebViewProvider": "interface",
    "android/webkit/WebViewDelegate": "class",
    "android/webkit/PacProcessor": "class",
    "android/webkit/TokenBindingService": "class",
    "android/webkit/WebViewFactory": "class",
    "android/webkit/WebView": "class",
}

# Nested types the provider references by descriptor; declared inside the stub
# of their owner above so javac can resolve `WebView.PrivateAccess` and friends.
STUB_NESTED = {
    "android/webkit/WebView": [
        "interface PrivateAccess",
        "interface VisualStateCallback",
        "interface FindListener",
        "interface PictureListener",
        "class HitTestResult",
    ],
    "android/webkit/WebViewFactory": ["class StartupTimestamps"],
    "android/webkit/WebViewFactoryProvider": ["interface Statics"],
    "android/webkit/WebViewDelegate": ["interface OnTraceEnabledChangeListener"],
}


def java_type(desc: str) -> str:
    if desc.startswith("["):
        return java_type(desc[1:]) + "[]"
    if desc.startswith("L") and desc.endswith(";"):
        return desc[1:-1].replace("/", ".").replace("$", ".")
    return PRIMITIVE[desc]


def parse_descriptors(args: str) -> list[str]:
    """`IILandroid/content/Intent;` -> ['I', 'I', 'Landroid/content/Intent;'].

    Class names contain the same letters as the primitive codes
    (`Landroid/content/Configuration;` has a `C`), so an `L` segment is scanned
    to its `;` instead of being split per character.
    """
    out: list[str] = []
    i = 0
    while i < len(args):
        start = i
        while args[i] == "[":
            i += 1
        if args[i] == "L":
            end = args.index(";", i)
            i = end + 1
        else:
            if args[i] not in "ZBCSIJFDV":
                raise SystemExit(f"bad type code {args[i]!r} in {args!r}")
            i += 1
        out.append(args[start:i])
    return out


def parse_methods() -> dict[str, list[tuple[str, list[str], str]]]:
    out: dict[str, list[tuple[str, list[str], str]]] = {}
    current = None
    with open(METHODS, encoding="utf-8") as handle:
        for line in handle:
            line = line.rstrip("\n")
            if line.startswith("=== L"):
                token = line[5:].strip()
                token = token[1:] if token.startswith("L") else token
                current = token.rstrip(";")
                out[current] = []
            elif current and line.startswith("  ") and " : (" in line:
                name, rest = line.strip().split(" : (", 1)
                args, ret = rest.rsplit(")", 1)
                out[current].append((name, parse_descriptors(args), ret))
    return out


def signature(name: str, args: list[str]) -> str:
    return f"{name}({''.join(args)})"


def stub_source(binary: str, kind: str, methods, nested_names, short_name=None) -> str:
    package, _, short = binary.rpartition("/")
    short = short_name or short
    lines = [
        f"package {package.replace('/', '.')};",
        "",
        "// GENERATED by tools/gen-webview-provider.py from the API-34 image's own dex.",
        "// COMPILE-TIME ONLY: the image's boot classpath supplies the real class at",
        "// runtime, so this source is never dexed into the provider APK.",
        f"public {kind} {short} {{",
    ]
    for entry in nested_names:
        nkind, nname = entry.split(" ", 1)
        lines.append(f"    public {nkind} {nname} {{}}")
    for method_name, args, ret in methods:
        if method_name in ("<init>", "<clinit>"):
            continue
        params = ", ".join(f"{java_type(a)} p{i}" for i, a in enumerate(args))
        if kind == "interface":
            lines.append(f"    {java_type(ret)} {method_name}({params});")
        else:
            rtype = java_type(ret)
            lines.append(
                f"    public {rtype} {method_name}({params}) {{ "
                f"{'' if rtype == 'void' else 'return '}{DEFAULT.get(rtype, 'null')}; }}"
            )
    lines.append("}")
    return "\n".join(lines) + "\n"


def provider_source(name: str, iface: str, methods, implemented, members="") -> str:
    lines = [
        f"package {PKG};",
        "",
        "// GENERATED by tools/gen-webview-provider.py: every method of the framework",
        "// interface is implemented. Implemented methods delegate to the provider's",
        "// real code; every other one throws a structured",
        "// UnsupportedOperationException naming the method and its dex descriptor",
        "// (ADR-0031; 0 false passes: never a silent null).",
        "import android.webkit.*;",
        "",
        f"public class {name} implements {iface} {{",
    ]
    short_iface = iface.rpartition(".")[2]
    if members:
        lines.append(members)
    for method_name, args, ret in methods:
        if method_name in ("<init>", "<clinit>"):
            continue
        params = ", ".join(f"{java_type(a)} p{i}" for i, a in enumerate(args))
        key = signature(method_name, args)
        body = implemented.get(key)
        if body is None:
            desc = f"({''.join(args)}){ret}"
            body = (
                "        throw new UnsupportedOperationException(\n"
                f'                "ACOS WebView provider: unsupported method '
                f'{short_iface}.{method_name}{desc}");'
            )
        lines.append(
            f"    @Override\n    public {java_type(ret)} {method_name}({params}) {{\n{body}\n    }}"
        )
    lines.append("}")
    return "\n".join(lines) + "\n"


def write(path: str, text: str, check: bool) -> bool:
    if check:
        try:
            with open(path, encoding="utf-8") as handle:
                if handle.read() == text:
                    return True
        except FileNotFoundError:
            pass
        print(f"gen-webview-provider: differs: {path}", file=sys.stderr)
        return False
    os.makedirs(os.path.dirname(path), exist_ok=True)
    with open(path, "w", encoding="utf-8") as handle:
        handle.write(text)
    return True


def main() -> int:
    check = "--check" in sys.argv
    methods = parse_methods()
    ok = True

    for binary, kind in STUB_CLASSES.items():
        nested = STUB_NESTED.get(binary, [])
        # A nested type that the dump lists as its own class (e.g.
        # `WebViewProvider$ViewDelegate`) is declared inside its owner.
        for listed in methods:
            if listed.startswith(binary + "$"):
                nested.append(f"interface {listed.split('$', 1)[1]}")
        nested_of: dict[str, list[tuple[str, list[str], str]]] = {}
        for listed, lmethods in methods.items():
            if listed.startswith(binary + "$"):
                nested_of[listed.split("$", 1)[1]] = lmethods
        src = stub_source(binary, kind, methods.get(binary, []), nested)
        if nested_of:
            # Fill the nested interfaces with their real method list.
            for nname, nmethods in nested_of.items():
                filled = stub_source(
                    binary, "interface", nmethods, [], short_name=nname
                )
                lines = filled.splitlines()
                body = "\n".join(lines[6:-1])  # inside `public interface <name> { ... }`
                src = src.replace(
                    f"    public interface {nname} {{}}",
                    f"    public interface {nname} {{\n{body}\n    }}",
                )
        ok &= write(os.path.join(STUBS, binary.rpartition("/")[2] + ".java"), src, check)

    view_delegate = provider_source(
        "WebViewChromiumViewDelegate",
        "WebViewProvider.ViewDelegate",
        methods["android/webkit/WebViewProvider$ViewDelegate"],
        {},
        members=(
            "    private final WebViewChromium mProvider;\n\n"
            "    WebViewChromiumViewDelegate(WebViewChromium provider) {\n"
            "        mProvider = provider;\n    }\n"
        ),
    )
    scroll_delegate = provider_source(
        "WebViewChromiumScrollDelegate",
        "WebViewProvider.ScrollDelegate",
        methods["android/webkit/WebViewProvider$ScrollDelegate"],
        {},
        members=(
            "    private final WebViewChromium mProvider;\n\n"
            "    WebViewChromiumScrollDelegate(WebViewChromium provider) {\n"
            "        mProvider = provider;\n    }\n"
        ),
    )
    view = provider_source(
        "WebViewChromium",
        "android.webkit.WebViewProvider",
        methods["android/webkit/WebViewProvider"],
        {
            "getViewDelegate()": "        return mViewDelegate;",
            "getScrollDelegate()": "        return mScrollDelegate;",
        },
        members=(
            "    private final WebViewChromiumViewDelegate mViewDelegate =\n"
            "            new WebViewChromiumViewDelegate(this);\n"
            "    private final WebViewChromiumScrollDelegate mScrollDelegate =\n"
            "            new WebViewChromiumScrollDelegate(this);\n\n"
            "    WebViewChromium() {\n    }\n"
        ),
    )
    factory = provider_source(
        "WebViewChromiumFactoryProvider",
        "android.webkit.WebViewFactoryProvider",
        methods["android/webkit/WebViewFactoryProvider"],
        {
            "createWebView(Landroid/webkit/WebView;Landroid/webkit/WebView$PrivateAccess;)":
                "        return new WebViewChromium();",
        },
        members=(
            "    static final String TAG = \"ACOSWvProvider\";\n"
            "    private final android.webkit.WebViewDelegate mDelegate;\n\n"
            "    WebViewChromiumFactoryProvider(android.webkit.WebViewDelegate delegate) {\n"
            "        mDelegate = delegate;\n    }\n"
        ),
    )
    for name, src in (
        ("WebViewChromiumViewDelegate", view_delegate),
        ("WebViewChromiumScrollDelegate", scroll_delegate),
        ("WebViewChromium", view),
        ("WebViewChromiumFactoryProvider", factory),
    ):
        ok &= write(os.path.join(SRC, PKG_DIR, name + ".java"), src, check)

    entry = os.path.join(SRC, PKG_DIR, "WebViewChromiumFactoryProviderForT.java")
    ok &= write(
        entry,
        f"""package {PKG};

// The class name the API-34 framework looks up by hand (MAX-TARGET-O hidden
// API): WebViewFactory.getWebViewProviderClass() does
// `Class.forName("com.android.webview.chromium.WebViewChromiumFactoryProviderForT",
// true, providerClassLoader)` and then invokes the static
// `create(WebViewDelegate)` reflectively, caching the WebViewFactoryProvider.
//
// The `ForT` suffix is the framework's own naming scheme (one class per
// provider-API generation); this provider targets the pinned API-34 image only,
// so exactly one entry point exists and a wrong image fails loudly at this call.
import android.util.Log;
import android.webkit.WebViewDelegate;
import android.webkit.WebViewFactoryProvider;

public final class WebViewChromiumFactoryProviderForT {{
    private WebViewChromiumFactoryProviderForT() {{}}

    public static WebViewFactoryProvider create(WebViewDelegate delegate) {{
        Log.i(WebViewChromiumFactoryProvider.TAG,
                "create(WebViewDelegate " + (delegate == null ? "null" : "present") + ")");
        return new WebViewChromiumFactoryProvider(delegate);
    }}
}}
""",
        check,
    )
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())
