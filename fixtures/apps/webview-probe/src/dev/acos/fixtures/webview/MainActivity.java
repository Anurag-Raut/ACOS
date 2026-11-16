package dev.acos.fixtures.webview;

import android.app.Activity;
import android.os.Bundle;
import android.os.Process;
import android.util.Log;
import android.view.Gravity;
import android.view.ViewGroup;
import android.webkit.JavascriptInterface;
import android.webkit.ValueCallback;
import android.webkit.WebChromeClient;
import android.webkit.WebSettings;
import android.webkit.WebView;
import android.webkit.WebViewClient;
import android.widget.LinearLayout;
import android.widget.ScrollView;
import android.widget.TextView;

/**
 * WV6 end-to-end probe: two real WebViews, two real pages, one engine.
 *
 * <p>Page A is served from this APK's assets through the broker's request
 * interception (`file:///android_asset/test.html` -> `req.intercept` -> the
 * guest's own `AssetManager`), page B is a `data:` URL. The probe records every
 * stage in the {@code status} TextView (and logcat tag {@code WVProbe}); the
 * scenario asserts the page text through the ACOS UI tree (the WIRE v1.1
 * `ax` -> `AccessibilityNodeProvider` path), taps the page's own button by that
 * text, and checks that the page's JS changed it and that the JS bridge ran.
 */
public class MainActivity extends Activity {
    private static final String TAG = "WVProbe";

    private static final String DATA_PAGE =
            "<!doctype html><html><head><meta charset='utf-8'><title>acos data page</title></head>"
            + "<body style='font:18px sans-serif'><h1 id=h style='font-size:24px;margin:8px 0'>acos data page</h1>"
            + "<div id=out style='font:16px sans-serif'>data page body</div></body></html>";

    private TextView status;
    private final StringBuilder log = new StringBuilder();

    /** A WebView that issues its navigation once the framework gave it a size. */
    private final class ProbeWebView extends WebView {
        private final String url;
        private final String label;
        private boolean loaded;

        ProbeWebView(android.content.Context context, String label, String url) {
            super(context);
            this.label = label;
            this.url = url;
        }

        @Override
        protected void onSizeChanged(int w, int h, int oldw, int oldh) {
            super.onSizeChanged(w, h, oldw, oldh);
            if (loaded || w <= 0 || h <= 0) {
                return;
            }
            loaded = true;
            append(label + " laid out " + w + "x" + h);
            // Post the navigation: `loadUrl` waits for the engine's load event,
            // and the launcher's first-traversal readiness must not be blocked
            // by a page (or its asset interception) on the UI thread.
            post(() -> {
                try {
                    loadUrl(url);
                    append(label + " loadUrl issued");
                } catch (Throwable t) {
                    append(label + " loadUrl FAILED: " + t);
                }
            });
        }
    }

    private static final class Bridge {
        private final MainActivity activity;

        Bridge(MainActivity activity) {
            this.activity = activity;
        }

        @JavascriptInterface
        public void report(String what) {
            activity.append("JS->Java: " + what);
        }
    }

    private void append(String line) {
        Log.i(TAG, line);
        runOnUiThread(() -> {
            log.append(line).append('\n');
            if (status != null) {
                status.setText(log.toString());
            }
        });
    }

    private ProbeWebView addWebView(LinearLayout root, int id, String label, String url) {
        ProbeWebView wv = new ProbeWebView(this, label, url);
        wv.setId(id);
        WebSettings settings = wv.getSettings();
        settings.setJavaScriptEnabled(true);
        wv.setWebViewClient(new WebViewClient() {
            @Override
            public void onPageFinished(WebView view, String loadedUrl) {
                String shown = loadedUrl != null && loadedUrl.startsWith("data:")
                        ? "data:text/html;base64,…" : String.valueOf(loadedUrl);
                append(label + " onPageFinished " + shown);
                view.evaluateJavascript(
                        "(function(){var h=document.getElementById('h');return h?h.textContent:''})()",
                        new ValueCallback<String>() {
                            @Override
                            public void onReceiveValue(String value) {
                                append(label + " evaluateJavascript -> " + value);
                            }
                        });
            }
        });
        wv.setWebChromeClient(new WebChromeClient());
        root.addView(wv, new LinearLayout.LayoutParams(900, 560));
        return wv;
    }

    @Override
    protected void onCreate(Bundle savedInstanceState) {
        super.onCreate(savedInstanceState);
        LinearLayout root = new LinearLayout(this);
        root.setOrientation(LinearLayout.VERTICAL);
        status = new TextView(this);
        status.setId(R.id.status);
        status.setTextSize(12f);
        status.setGravity(Gravity.START);
        status.setText("uid=" + Process.myUid() + " pid=" + Process.myPid() + "\n");
        // Fixed height: the probe's own log must not push the WebViews around
        // (a wrap-content status TextView outgrew the screen and left page A
        // clipped). The full text is still the node's `text` for assertions.
        root.addView(status, new LinearLayout.LayoutParams(
                ViewGroup.LayoutParams.MATCH_PARENT, 420));
        ScrollView scroll = new ScrollView(this);
        scroll.addView(root);
        setContentView(scroll);

        try {
            append("hasSystemFeature(android.software.webview)="
                    + getPackageManager().hasSystemFeature("android.software.webview"));
            append("WebView.getCurrentWebViewPackage()=" + WebView.getCurrentWebViewPackage());

            // Page A: the broker intercepts file:///android_asset/test.html and
            // asks this process for the APK asset bytes (WIRE.md §4.3).
            ProbeWebView a = addWebView(root, R.id.webview, "A", "file:///android_asset/test.html");
            append("WebView created: " + a);
            a.addJavascriptInterface(new Bridge(this), "acosBridge");

            // Page B: a data: URL; Chromium never network-fetches it, so it
            // needs no interception at all (WIRE.md §4.3).
            String encoded = android.util.Base64.encodeToString(
                    DATA_PAGE.getBytes("UTF-8"), android.util.Base64.NO_WRAP);
            ProbeWebView b = addWebView(root, R.id.webview2,
                    "B", "data:text/html;base64," + encoded);
            append("WebView created: " + b);
        } catch (Throwable t) {
            StringBuilder where = new StringBuilder();
            StackTraceElement[] st = t.getStackTrace();
            for (int i = 0; i < Math.min(5, st.length); i++) {
                where.append("\n  at ").append(st[i]);
            }
            append("WebView FAILED: " + t.getClass().getName() + ": " + t.getMessage() + where);
        }
    }
}
