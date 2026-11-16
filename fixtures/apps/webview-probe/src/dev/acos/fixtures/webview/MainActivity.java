package dev.acos.fixtures.webview;

import android.app.Activity;
import android.os.Bundle;
import android.os.Process;
import android.util.Log;
import android.view.Gravity;
import android.view.ViewGroup;
import android.webkit.JavascriptInterface;
import android.webkit.ValueCallback;
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
    private final StringBuilder log = new StringBuilder();

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

        try {
            WebView wv = new WebView(this);
            append("WebView created: " + wv);
            WebSettings settings = wv.getSettings();
            settings.setJavaScriptEnabled(true);
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
            wv.loadDataWithBaseURL("https://acos.invalid/probe", PAGE, "text/html", "UTF-8", null);
            wv.setId(R.id.webview);
            root.addView(wv, new LinearLayout.LayoutParams(900, 700));
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
