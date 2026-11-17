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

    private static final String DATA_PAGE_B64;
    static {
        String b64 = "";
        try {
            b64 = android.util.Base64.encodeToString(
                    DATA_PAGE.getBytes("UTF-8"), android.util.Base64.NO_WRAP);
        } catch (Exception e) {
            b64 = "";
        }
        DATA_PAGE_B64 = b64;
    }

    /**
     * WVFAIR: the deliberately slow page. It loads instantly and then, on the
     * first timer tick, repaints a full-viewport canvas with 600 shadowed rects
     * every animation frame and keeps the engine's main thread busy in 1900 ms
     * slices — so one `beginFrame` on it costs ~2 s while the other WebView is
     * healthy. The page runs in the **host** engine (the guest only pulls
     * frames), which is exactly the contention the broker's fair frame gate has
     * to bound.
     */
    private static final String SLOW_PAGE =
            "<!doctype html><html><head><meta charset='utf-8'><title>acos slow page</title></head>"
            + "<body style='margin:0;background:#111'>"
            + "<canvas id=c width=900 height=560 style='filter:blur(3px)'></canvas>"
            + "<div id=out style='color:#fff;font:16px sans-serif'>slow page</div>"
            + "<script>setTimeout(function(){"
            + "var g=document.getElementById('c').getContext('2d');var n=0;"
            + "(function paint(){for(var i=0;i<600;i++){g.shadowBlur=40;"
            + "g.shadowColor='rgba(255,0,0,0.7)';"
            + "g.fillStyle='hsl('+((n+i)*7%360)+',85%,55%)';"
            + "g.fillRect((i*37+n*3)%900,(i*23)%560,300,220);}n++;"
            + "requestAnimationFrame(paint);})();"
            + "setInterval(function(){var t=performance.now();"
            + "while(performance.now()-t<1900){}},0);"
            + "},0);</script></body></html>";

    private static final String SLOW_PAGE_B64;
    static {
        String b64 = "";
        try {
            b64 = android.util.Base64.encodeToString(
                    SLOW_PAGE.getBytes("UTF-8"), android.util.Base64.NO_WRAP);
        } catch (Exception e) {
            b64 = "";
        }
        SLOW_PAGE_B64 = b64;
    }

    private static void loadData(WebView view, String base64) {
        view.loadUrl("data:text/html;base64," + base64);
    }

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
            if ("tapped".equals(what) && activity.mFairArm) {
                long down = activity.mTapDownNanos;
                long ms = down == 0 ? -1 : (System.nanoTime() - down) / 1_000_000L;
                activity.append("fair: tap -> page click " + ms + " ms");
                return;
            }
            if ("tapped".equals(what)) {
                // The page's own click: the input steps are done, so the
                // viewport arms can run without starving them.
                new android.os.Handler(android.os.Looper.getMainLooper())
                        .post(activity::startViewportArms);
            }
        }
    }

    // -- WV8 viewport arms -------------------------------------------------
    //
    // One extra WebView carries every arm so the probe stays at two views: a
    // session with four engine targets serialises so many frames on the broker
    // that the tap's guest ack arrived 2 s late (measured) and the input was
    // lost. The arms are therefore sequential, and each one is a real load:
    //
    //   1. `B` is created with `setUseWideViewPort(true)` **before** its first
    //      load (where an app sets it), so the provider replays the switch right
    //      after `view.create` — the page has `<meta viewport content='width=640'>`
    //      and lays out at 640 CSS px.
    //   2. the same view loads a page without a meta tag: the wide arm's default
    //      is Chromium's 980 CSS px.
    //   3. the switch goes back to the default (narrow) and the same page is
    //      loaded again: the layout viewport is the View's CSS width.
    // -- WVFAIR: the two-WebView fairness arm ---------------------------------
    //
    // `scenarios/webview-fair.yaml` taps the probe's own "start slow page in B"
    // button: page A stays the cheap tappable page and B loads the deliberately
    // slow page. The probe then times the next tap end to end — MotionEvent down
    // -> the page's own click JS reaching the Java bridge — and logs it, so the
    // scenario asserts a *number*: before the broker's fair gate this took ~2 s
    // and the click was sometimes lost (WV8 §5). The fair gate keeps it at the
    // cost of one bounded beginFrame attempt.
    private volatile boolean mFairArm;
    private ProbeWebView mSlowView;
    private ProbeWebView mFastView;
    private volatile long mTapDownNanos;

    private volatile int mViewportArm;
    private volatile boolean mViewportArmsStarted;
    private ProbeWebView mViewportView;

    /**
     * The arms start when the page's own button calls the Java bridge
     * (`window.acosBridge.report("tapped")`), i.e. after the scenario's input
     * steps. Two engine targets that are both loading pages serialise their
     * frame pulls on the broker, and the guest's UI thread blocks in `onDraw`
     * waiting for one: the tap's own ack arrived ~2 s late and the click was
     * lost (`Choreographer: Skipped 119 frames`). Input fidelity is measured
     * first, then the viewport arms run on the same connection.
     */
    private void startViewportArms() {
        if (mViewportArmsStarted) {
            return;
        }
        mViewportArmsStarted = true;
        append("viewport arms: start (after the tap)");
        advanceViewportArm();
    }

    private void advanceViewportArm() {
        final WebView view = mViewportView;
        if (view == null) {
            return;
        }
        final int arm = ++mViewportArm;
        new android.os.Handler(android.os.Looper.getMainLooper()).post(() -> {
            try {
                if (arm == 1) {
                    append("arm 2: wide, page without <meta viewport>");
                    loadData(view, DATA_PAGE_B64);
                } else if (arm == 2) {
                    append("arm 3: back to the default (narrow) arm");
                    view.getSettings().setUseWideViewPort(false);
                    view.getSettings().setLoadWithOverviewMode(false);
                    loadData(view, DATA_PAGE_B64);
                }
            } catch (Throwable t) {
                append("viewport arm " + arm + " FAILED: " + t);
            }
        });
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

    /**
     * WVFAIR: the fairness arm's page tap needs an *immediate* native reaction.
     *
     * A tap step asserts that the screen changed within its settle window, and
     * the page's own reaction is a virtual node inside the WebView — with the
     * second WebView pulling frames it arrives one bounded engine attempt later
     * (~1.5 s on ART), after the step's window. The touch listener is the honest
     * "the app received this touch" marker: it changes the status text at once
     * and lets the WebView handle the gesture unchanged (`return false`). The
     * page's click and its end-to-end latency are still asserted separately.
     */
    private void watchFairTouch(ProbeWebView view) {
        view.setOnTouchListener((v, event) -> {
            if (event.getActionMasked() == android.view.MotionEvent.ACTION_DOWN) {
                // Posted, not written inline: a change made *inside* the touch
                // dispatch is already in the tap step's baseline sample, and the
                // step's state-change rule then sees "nothing changed". The
                // marker is appended a few times over ~0.8 s so it is *inside*
                // the step's content-stable window whatever the exact baseline
                // sample was.
                for (long delay : new long[] {100, 400, 800}) {
                    v.postDelayed(() -> append("fair: touch down on A"), delay);
                }
            }
            return false;
        });
    }

    private ProbeWebView addWebView(LinearLayout root, int id, String label, String url) {
        return addWebView(root, id, label, url, false, false);
    }

    /**
     * WV8: the viewport arms. `wide` sets `useWideViewPort` (and
     * `loadWithOverviewMode`) **before** the first load — i.e. before the
     * provider created its engine view, which is where an app normally sets
     * them — and `reportViewport` asks the page for its layout width and page
     * scale, which is the assertion the scenario makes.
     */
    private ProbeWebView addWebView(
            LinearLayout root, int id, String label, String url, boolean wide, boolean reportViewport) {
        ProbeWebView wv = new ProbeWebView(this, label, url);
        wv.setId(id);
        WebSettings settings = wv.getSettings();
        settings.setJavaScriptEnabled(true);
        if (wide) {
            settings.setUseWideViewPort(true);
            settings.setLoadWithOverviewMode(true);
        }
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
                if (reportViewport) {
                    // The layout viewport width and the page scale, straight from
                    // the page: `clientWidth` is the CSS px the document is laid
                    // out at, `visualViewport.scale` is what overview mode did.
                    view.evaluateJavascript(
                            "(function(){var w=document.documentElement.clientWidth;"
                            + "var s=(window.visualViewport?window.visualViewport.scale:1);"
                            + "return w+'x'+s.toFixed(2)})()",
                            new ValueCallback<String>() {
                                @Override
                                public void onReceiveValue(String value) {
                                    if (label.equals("B")) {
                                        append(label + " viewport#" + mViewportArm + " -> " + value);
                                        // #0 is the create-path arm (before the
                                        // tap); every later report advances the
                                        // sequence.
                                        if (mViewportArmsStarted) {
                                            advanceViewportArm();
                                        }
                                    } else {
                                        append(label + " viewport -> " + value);
                                    }
                                }
                            });
                }
            }
        });
        wv.setWebChromeClient(new WebChromeClient());
        root.addView(wv, new LinearLayout.LayoutParams(900, 560));
        return wv;
    }

    /** The tap's own down time: the start of the end-to-end input measurement. */
    @Override
    public boolean dispatchTouchEvent(android.view.MotionEvent event) {
        if (event.getActionMasked() == android.view.MotionEvent.ACTION_DOWN) {
            mTapDownNanos = System.nanoTime();
        }
        return super.dispatchTouchEvent(event);
    }

    /**
     * WVFAIR: after page A has loaded, put the slow page in B and leave A
     * tappable. The scenario then taps A's button while B is at its most
     * expensive and asserts both the page's own click and its bounded latency.
     */
    private void startFairArm() {
        if (mFairArm) {
            return;
        }
        mFairArm = true;
        append("fair: slow page started in B");
        if (mSlowView != null) {
            loadData(mSlowView, SLOW_PAGE_B64);
        }
        ProbeWebView fast = mFastView;
        if (fast != null) {
            watchFairTouch(fast);
        }
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
            ProbeWebView a = addWebView(
                    root, R.id.webview, "A", "file:///android_asset/test.html", false, true);
            mFastView = a;
            append("WebView created: " + a);
            a.addJavascriptInterface(new Bridge(this), "acosBridge");

            // Page B (WV8): the viewport arms. Created with the wide switch on
            // *before* the first load, and pointed at a page that declares
            // `<meta name="viewport" content="width=640">`; `advanceViewportArm`
            // then walks the remaining arms (see the comment there).
            ProbeWebView b = addWebView(root, R.id.webview2,
                    "B", "file:///android_asset/viewport-meta.html", true, true);
            mViewportView = b;
            mSlowView = b;
            append("WebView created: " + b);
            // WVFAIR: the fairness arm's trigger. Tapping this loads the
            // deliberately slow page in B, so the *next* tap on A happens while
            // the engine is being kept busy by the second WebView. The WV8
            // scenario never taps it, so B keeps its viewport arms there.
            android.widget.TextView slow = new android.widget.TextView(this);
            slow.setId(R.id.slow);
            slow.setText("start slow page in B");
            slow.setTextSize(14f);
            slow.setPadding(8, 8, 8, 8);
            slow.setOnClickListener(v -> {
                // The button's own text is the tap's visible effect (a tap step
                // asserts the tree changed); the slow page then starts in B.
                slow.setText("slow page started in B");
                startFairArm();
            });
            root.addView(slow, new LinearLayout.LayoutParams(
                    ViewGroup.LayoutParams.MATCH_PARENT, 60));
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
