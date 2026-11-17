package com.android.webview.chromium;

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

public final class WebViewChromiumFactoryProviderForT {
    private WebViewChromiumFactoryProviderForT() {}

    public static WebViewFactoryProvider create(WebViewDelegate delegate) {
        Log.i(WebViewChromiumFactoryProvider.TAG,
                "create(WebViewDelegate " + (delegate == null ? "null" : "present") + ")");
        return new WebViewChromiumFactoryProvider(delegate);
    }
}
