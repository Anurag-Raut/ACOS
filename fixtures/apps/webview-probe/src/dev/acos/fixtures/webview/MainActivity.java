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
 * SPIKE-WV probe: what does the guest do today when an app creates a WebView?
 *
 * The whole point is the reported failure, so every stage reports its outcome
 * into a TextView (id=status, visible in the ACOS UI tree) and into logcat with
 * the tag "WVProbe". Nothing is swallowed: the exact throwable class, message
 * and top stack frame are printed.
 */
public class MainActivity extends Activity {
    private static final String TAG = "WVProbe";
    private static final String PAGE =
            "<!doctype html><html><body style='font:32px sans-serif'>"
            + "<h1 id=h>acos webview</h1>"
            + "<button id=b style='font:32px sans-serif' onclick=\"document.getElementById('out')"
            + ".textContent='tapped';window.acosBridge.report('tapped')\">Tap me</button>"
            + "<div id=out style='font:28px sans-serif'>not tapped</div></body></html>";

    private TextView status;
    private ProbeWebView webView;
    private final StringBuilder log = new StringBuilder();

    /**
     * WV5a: a WebView the probe can observe after layout. `onSizeChanged` is the
     * framework's own "the view now has its real size" callback, so the
     * navigation is issued from there: the engine view is created at measure
     * (`view.create`) and resized at layout (`view.setViewport`) *before*
     * `view.loadUrl`, exactly the order a real app produces.
     */
    private final class ProbeWebView extends WebView {
        private boolean loaded;

        ProbeWebView(android.content.Context context) {
            super(context);
        }

        @Override
        protected void onSizeChanged(int w, int h, int oldw, int oldh) {
            super.onSizeChanged(w, h, oldw, oldh);
            if (loaded || w <= 0 || h <= 0) {
                return;
            }
            loaded = true;
            append("laid out " + w + "x" + h);
            try {
                // A `data:` URL goes through `view.loadUrl` (`WIRE.md` §4.2) and
                // needs no network: Chromium never network-fetches `data:` (§4.3).
                loadUrl("data:text/html;charset=utf-8,"
                        + java.net.URLEncoder.encode(PAGE, "UTF-8"));
                append("loadUrl issued");
            } catch (Throwable t) {
                append("loadUrl FAILED: " + t);
            }
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

    @Override
    protected void onCreate(Bundle savedInstanceState) {
        super.onCreate(savedInstanceState);
        LinearLayout root = new LinearLayout(this);
        root.setOrientation(LinearLayout.VERTICAL);
        status = new TextView(this);
        status.setId(R.id.status);
        status.setTextSize(14f);
        status.setGravity(Gravity.START);
        status.setText("uid=" + Process.myUid() + " pid=" + Process.myPid() + "\n");
        root.addView(status, new LinearLayout.LayoutParams(
                ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.WRAP_CONTENT));
        ScrollView scroll = new ScrollView(this);
        scroll.addView(root);
        setContentView(scroll);

        String feature;
        try {
            feature = String.valueOf(getPackageManager().hasSystemFeature("android.software.webview"));
        } catch (Throwable t) {
            feature = "hasSystemFeature threw " + t;
        }
        append("hasSystemFeature(android.software.webview)=" + feature);
        append("WebView.getCurrentWebViewPackage()=" + WebView.getCurrentWebViewPackage());
        append("myUid=" + Process.myUid());

        // SPIKE-WV diagnostic: `WebViewFactory` resolves the provider package
        // with `getPackageInfo(pkg, GET_SIGNATURES|GET_META_DATA|
        // GET_SHARED_LIBRARY_FILES|MATCH_UNINSTALLED_PACKAGES|0x10000000)`.
        // Ask for the *app's own* package with the same flags: it isolates
        // "the PMS reply for these flags is broken" from "the provider record
        // is broken".
        try {
            android.content.pm.PackageInfo own = getPackageManager().getPackageInfo(
                    getPackageName(),
                    (int) 0x100024c0);
            append("own getPackageInfo(0x100024c0) -> " + own.packageName
                    + " sigs=" + (own.signatures == null ? "null" : own.signatures.length));
        } catch (Throwable t) {
            append("own getPackageInfo(0x100024c0) FAILED: " + t);
        }

        try {
            ProbeWebView wv = new ProbeWebView(this);
            append("WebView created: " + wv);
            WebSettings settings = wv.getSettings();
            settings.setJavaScriptEnabled(true);
            append("settings ok: jsEnabled=" + settings.getJavaScriptEnabled());
            wv.addJavascriptInterface(new Bridge(this), "acosBridge");
            wv.setWebViewClient(new WebViewClient() {
                @Override
                public void onPageFinished(WebView view, String url) {
                    append("onPageFinished " + url);
                    view.evaluateJavascript("document.getElementById('h').textContent",
                            new ValueCallback<String>() {
                                @Override
                                public void onReceiveValue(String value) {
                                    append("evaluateJavascript -> " + value);
                                }
                            });
                }
            });
            wv.setWebChromeClient(new WebChromeClient());
            append("clients set");
            // WV5a: attach and lay the view out *before* the navigation, so the
            // engine view is created and resized with the View's real geometry
            // (`view.create` at measure, `view.setViewport` at layout) before
            // `view.loadUrl` is issued — the same order a real page sees, and the
            // one the session listener asserts. The load happens from
            // `onSizeChanged`, i.e. from the fixture's own layout callback, not
            // from a guessed delay.
            this.webView = wv;
            wv.setId(R.id.webview);
            root.addView(wv, new LinearLayout.LayoutParams(900, 700));
            append("attached (layout pending)");
        } catch (Throwable t) {
            StringBuilder where = new StringBuilder();
            StackTraceElement[] st = t.getStackTrace();
            for (int i = 0; i < Math.min(4, st.length); i++) {
                where.append("\n  at ").append(st[i]);
            }
            append("WebView FAILED: " + t.getClass().getName() + ": " + t.getMessage() + where);
        }
    }
}
