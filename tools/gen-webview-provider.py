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
WEBSETTINGS = os.path.join(ROOT, "research/spike-wv/provider/WEBSETTINGS.md")
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


def parse_websettings() -> list[tuple[str, list[str], str]]:
    """`WEBSETTINGS.md` -> [(name, [paramType], returnType)].

    `WebSettings` is public API, so this list does not come from the image's dex
    (`METHODS.md` does): it is `javap` over the SDK's android.jar, which is what
    the provider is compiled against. See the file's own header.
    """
    out: list[tuple[str, list[str], str]] = []
    with open(WEBSETTINGS, encoding="utf-8") as handle:
        for line in handle:
            line = line.strip()
            if not line or line.startswith("#"):
                continue
            decl, ret = line.rsplit(")", 1)
            name, params = decl.split("(", 1)
            out.append((name, [p for p in params.split(",") if p], ret))
    return out


def settings_source(methods, implemented) -> str:
    """`WebSettingsChromium`: guest state, or an honest refusal per method."""
    lines = [
        f"package {PKG};",
        "",
        "// GENERATED by tools/gen-webview-provider.py from the SDK's android.jar",
        "// (`WEBSETTINGS.md`): every abstract method of `android.webkit.WebSettings`",
        "// is implemented. The ones WIRE v1 can honour (`javascriptEnabled` via",
        "// `view.create`, the user agent via `view.setUserAgent`) forward to the",
        "// provider's real code; every other one throws a structured",
        "// UnsupportedOperationException naming the method, because WIRE v1 has no",
        "// message for it (results/handoff/WV5a/WIRE-ADDITIONS.md §2). A getter that",
        "// returned a made-up default would be a false success.",
        "import android.webkit.*;",
        "",
        "public class WebSettingsChromium extends android.webkit.WebSettings {",
        "    private final WebViewChromiumCore mCore;",
        "",
        "    WebSettingsChromium(WebViewChromiumCore core) {",
        "        mCore = core;",
        "    }",
    ]
    for name, args, ret in methods:
        params = ", ".join(f"{a} p{i}" for i, a in enumerate(args))
        key = f"{name}({','.join(args)})"
        body = implemented.get(key)
        if body is None:
            desc = f"{ret} {name}({', '.join(args)})"
            lines.append(
                "    @Override\n"
                f"    public {ret} {name}({params}) {{\n"
                "        throw new UnsupportedOperationException(\n"
                '                "ACOS WebView provider: unsupported method WebSettings: '
                f'{desc}; WIRE v1 has no message for it (see '
                'results/handoff/WV5a/WIRE-ADDITIONS.md)");\n'
                "    }"
            )
        else:
            lines.append(
                f"    @Override\n    public {ret} {name}({params}) {{\n{body}\n    }}"
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
        if binary == "android/webkit/WebView":
            # WV2: the provider's View side calls `getWidth`/`getHeight`/
            # `getResources`/`postInvalidateOnAnimation`, which the stub did not
            # declare. On the device `WebView` **is** an `AbsoluteLayout` (a
            # `View`), so the compile-time stub extends `View`: the same
            # members the framework provides, never a fabricated API.
            src = src.replace(
                "public class WebView {",
                "public class WebView extends android.view.View {\n"
                "    public WebView(android.content.Context context) { super(context); }",
                1,
            )
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
        {
            # WV5a (wall 9): the surface the framework's constructor and first
            # measure/layout/draw reach. Geometry, painting, input and focus are
            # the guest's hooks into the core (which owns the broker transport);
            # each one is either guest-only View state (what Chromium's own
            # AwContents keeps) or a WIRE v1 request, and the split is documented
            # in WebViewChromiumCore.
            "onDraw(Landroid/graphics/Canvas;)":
                "        mProvider.core().onDraw(p0);",
            "onSizeChanged(IIII)":
                "        mProvider.core().onSizeChanged(p0, p1, p2, p3);",
            "onTouchEvent(Landroid/view/MotionEvent;)":
                "        return mProvider.core().onTouchEvent(p0);",
            "onAttachedToWindow()":
                "        mProvider.core().attachToWindow();",
            "onDetachedFromWindow()":
                "        mProvider.core().detachFromWindow();",
            "setOverScrollMode(I)":
                "        mProvider.core().setOverScrollMode(p0);",
            "setLayerType(ILandroid/graphics/Paint;)":
                "        mProvider.core().setLayerType(p0, p1);",
            "setLayoutParams(Landroid/view/ViewGroup$LayoutParams;)":
                "        mProvider.core().setLayoutParams(p0);",
            "setBackgroundColor(I)":
                "        mProvider.core().setBackgroundColor(p0);",
            "setScrollBarStyle(I)":
                "        mProvider.core().setScrollBarStyle(p0);",
            "onMeasure(II)":
                "        mProvider.core().onMeasure(p0, p1);",
            "setFrame(IIII)":
                "        return mProvider.core().setFrame(p0, p1, p2, p3);",
            "onVisibilityChanged(Landroid/view/View;I)":
                "        mProvider.core().onVisibilityChanged(p0, p1);",
            "onWindowVisibilityChanged(I)":
                "        mProvider.core().onWindowVisibilityChanged(p0);",
            "onWindowFocusChanged(Z)":
                "        mProvider.core().onWindowFocusChanged(p0);",
            "onFocusChanged(ZILandroid/graphics/Rect;)":
                "        mProvider.core().onFocusChanged(p0, p1, p2);",
            "requestFocus(ILandroid/graphics/Rect;)":
                "        return mProvider.core().requestFocus(p0, p1);",
            "onScrollChanged(IIII)":
                "        mProvider.core().onScrolled(p0, p1);",
            "onConfigurationChanged(Landroid/content/res/Configuration;)":
                "        mProvider.core().onConfigurationChanged(p0);",
            "onMovedToDisplay(ILandroid/content/res/Configuration;)":
                "        mProvider.core().onMovedToDisplay(p0, p1);",
            "getHandler(Landroid/os/Handler;)":
                # Chromium hands the framework's own handler back; there is no
                # provider thread of its own in v0.4.0 (the transport runs its
                # own daemon threads and posts UI work through the core).
                "        return p0;",
            "findFocus(Landroid/view/View;)":
                "        return mProvider.core().findFocus(p0);",
            "onCheckIsTextEditor()":
                "        return mProvider.core().isTextEditor();",
            "onCreateInputConnection(Landroid/view/inputmethod/EditorInfo;)":
                "        return mProvider.core().onCreateInputConnection(p0);",
            "onApplyWindowInsets(Landroid/view/WindowInsets;)":
                # The WebView consumes no insets: it returns them unchanged, the
                # same answer a plain View gives.
                "        return p0;",
            "onDragEvent(Landroid/view/DragEvent;)":
                # `false` is the framework's "this view did not handle the drag".
                "        return false;",
            "shouldDelayChildPressedState()":
                "        return true;",
            "preDispatchDraw(Landroid/graphics/Canvas;)":
                "        // Nothing is drawn behind the page: the frame is `onDraw`'s.",
            "onDrawVerticalScrollBar(Landroid/graphics/Canvas;Landroid/graphics/drawable/Drawable;IIII)":
                "        // Scrollbars are the engine's overlay; the guest draws none.",
            "onStartTemporaryDetach()":
                "        // View-recycling bookkeeping; nothing engine-side changes.",
            "onFinishTemporaryDetach()":
                "        // View-recycling bookkeeping; nothing engine-side changes.",
            "onInitializeAccessibilityNodeInfo(Landroid/view/accessibility/AccessibilityNodeInfo;)":
                "        mProvider.core().onInitializeAccessibilityNodeInfo(p0);",
            "onInitializeAccessibilityEvent(Landroid/view/accessibility/AccessibilityEvent;)":
                "        mProvider.core().onInitializeAccessibilityEvent(p0);",
            "getAccessibilityNodeProvider()":
                # The page's virtual accessibility tree is engine state (WIRE v1
                # §4.4 `ax`), and mapping it onto an
                # `AccessibilityNodeProvider` has not landed. `null` would tell
                # the platform "this view has no virtual children", which is a
                # false claim about a page, so this refuses and names itself.
                "        throw new UnsupportedOperationException(\n"
                '                "ACOS WebView provider: unsupported method '
                'ViewDelegate.getAccessibilityNodeProvider()Landroid/view/accessibility/'
                'AccessibilityNodeProvider; (the page AX tree is the WIRE v1 op ax, not yet '
                'mapped; see results/handoff/WV5a/WIRE-ADDITIONS.md §5)");',
        },
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
        {
            "computeScroll()":
                "        mProvider.core().computeScroll();",
            "computeHorizontalScrollRange()":
                "        return mProvider.core().computeHorizontalScrollRange();",
            "computeHorizontalScrollOffset()":
                "        return mProvider.core().computeHorizontalScrollOffset();",
            "computeVerticalScrollRange()":
                "        return mProvider.core().computeVerticalScrollRange();",
            "computeVerticalScrollOffset()":
                "        return mProvider.core().computeVerticalScrollOffset();",
            "computeVerticalScrollExtent()":
                "        return mProvider.core().computeVerticalScrollExtent();",
        },
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
            # WV5a: the constructor's `ensureProviderCreated()` is followed by
            # `init(...)`, and the fixture needs `getSettings()` plus the chrome
            # client's guest-side slot.
            "init(Ljava/util/Map;Z)":
                "        mCore.init(p0, p1);",
            "getSettings()": "        return mCore.settings();",
            "setWebChromeClient(Landroid/webkit/WebChromeClient;)":
                "        mCore.setWebChromeClient(p0);",
            "getWebChromeClient()": "        return mCore.chromeClient();",
            "getTitle()": "        return mCore.currentTitle();",
            "loadUrl(Ljava/lang/String;)": "        mCore.loadUrl(p0);",
            "loadUrl(Ljava/lang/String;Ljava/util/Map;)":
                "        mCore.loadUrl(p0);",
            "loadData(Ljava/lang/String;Ljava/lang/String;Ljava/lang/String;)":
                "        mCore.loadData(p0, p1, p2);",
            "loadDataWithBaseURL(Ljava/lang/String;Ljava/lang/String;Ljava/lang/String;Ljava/lang/String;Ljava/lang/String;)":
                "        mCore.loadDataWithBaseUrl(p0, p1, p2, p3, p4);",
            "evaluateJavaScript(Ljava/lang/String;Landroid/webkit/ValueCallback;)":
                "        mCore.evaluateJavascript(p0, p1);",
            "addJavascriptInterface(Ljava/lang/Object;Ljava/lang/String;)":
                "        mCore.addJavascriptInterface(p0, p1);",
            "removeJavascriptInterface(Ljava/lang/String;)":
                "        mCore.removeJavascriptInterface(p0);",
            "reload()": "        mCore.reload();",
            "stopLoading()": "        mCore.stopLoading();",
            "destroy()": "        mCore.destroy();",
            "setWebViewClient(Landroid/webkit/WebViewClient;)":
                "        mCore.setWebViewClient(p0);",
            "getWebViewClient()": "        return mCore.client();",
            "getUrl()": "        return mCore.currentUrl();",
            "setNetworkAvailable(Z)": "        mCore.setNetworkAvailable(p0);",
        },
        members=(
            "    private final WebViewChromiumViewDelegate mViewDelegate =\n"
            "            new WebViewChromiumViewDelegate(this);\n"
            "    private final WebViewChromiumScrollDelegate mScrollDelegate =\n"
            "            new WebViewChromiumScrollDelegate(this);\n"
            "    private final WebViewChromiumCore mCore;\n\n"
            "    WebViewChromium(WebViewChromiumCore core) {\n"
            "        mCore = core;\n    }\n\n"
            "    WebViewChromiumCore core() {\n        return mCore;\n    }\n"
        ),
    )
    factory = provider_source(
        "WebViewChromiumFactoryProvider",
        "android.webkit.WebViewFactoryProvider",
        methods["android/webkit/WebViewFactoryProvider"],
        {
            "createWebView(Landroid/webkit/WebView;Landroid/webkit/WebView$PrivateAccess;)":
                "        WebViewChromiumCore core = new WebViewChromiumCore(transport(p0));\n"
                "        core.attach(p0);\n"
                "        return new WebViewChromium(core);",
        },
        members=(
            "    static final String TAG = \"ACOSWvProvider\";\n"
            "    /** WIRE.md §0: the session socket ACOS bind-mounts into the guest. */\n"
            "    static final String SOCKET_PATH = \"/run/acos/webview.sock\";\n"
            "    private final android.webkit.WebViewDelegate mDelegate;\n"
            "    private AcosWebViewTransport.Transport mTransport;\n\n"
            "    WebViewChromiumFactoryProvider(android.webkit.WebViewDelegate delegate) {\n"
            "        mDelegate = delegate;\n    }\n\n"
            "    /**\n"
            "     * The session transport, created once. The stand-in is a connected\n"
            "     * stream pair over `SOCKET_PATH`; the broker client satisfies the same\n"
            "     * interface (`AcosWebViewTransport.Transport`), so wiring it in does\n"
            "     * not touch the provider.\n"
            "     */\n"
            "    synchronized AcosWebViewTransport.Transport transport(android.webkit.WebView view) {\n"
            "        if (mTransport == null) {\n"
            "            mTransport = AcosSessionSocket.open(SOCKET_PATH);\n"
            "        }\n"
            "        return mTransport;\n"
            "    }\n"
        ),
    )
    for name, src in (
        ("WebViewChromiumViewDelegate", view_delegate),
        ("WebViewChromiumScrollDelegate", scroll_delegate),
        ("WebViewChromium", view),
        ("WebViewChromiumFactoryProvider", factory),
    ):
        ok &= write(os.path.join(SRC, PKG_DIR, name + ".java"), src, check)

    write(
        os.path.join(SRC, PKG_DIR, "WebSettingsChromium.java"),
        settings_source(
            parse_websettings(),
            {
                # WIRE v1's `view.create` carries `javascriptEnabled` and WIRE's
                # `view.setUserAgent` carries the user agent (§4.2); every other
                # WebSettings method has no wire message and stays an honest
                # refusal (results/handoff/WV5a/WIRE-ADDITIONS.md §2).
                "setJavaScriptEnabled(boolean)":
                    "        mCore.setJavascriptEnabled(p0);",
                "getJavaScriptEnabled()":
                    "        return mCore.javascriptEnabled();",
                "setUserAgentString(java.lang.String)":
                    "        mCore.setUserAgent(p0);",
                "getUserAgentString()":
                    "        return mCore.userAgent();",
            },
        ),
        check,
    )
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
