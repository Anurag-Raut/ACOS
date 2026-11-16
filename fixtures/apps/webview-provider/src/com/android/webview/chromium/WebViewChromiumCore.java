package com.android.webview.chromium;

// WV2: the provider's real code. The generated `WebViewChromium` /
// `WebViewChromiumViewDelegate` / `WebViewChromiumScrollDelegate` classes only
// forward their framework-interface methods here, so the framework surface and
// the behaviour stay apart: the generator owns the 105-method contract, this
// class owns what a WebView actually does.
//
// Everything is expressed as wire calls (results/handoff/WVB/WIRE.md, §4):
// there is no in-process engine, no DevTools, no second process — the host
// broker owns Chromium and this class is a client of it.
//
// Android semantics kept on the guest side, because they are the app-visible
// contract rather than engine work:
//   * `WebViewClient.onPageStarted` / `onPageFinished` fire from the broker's
//     `navigation.*` events, on the UI thread, in event order;
//   * `shouldOverrideUrlLoading` is a *host request* the broker makes before a
//     navigation commits (`nav.shouldOverride`), answered with `allow`;
//   * `onDraw` paints the last decoded frame at the View's own size/density
//     (no scaling in the engine), so a stale frame stays on screen instead of
//     tearing;
//   * `onTouchEvent` forwards the raw pointer stream, `onSizeChanged` sets the
//     viewport — the engine never guesses the guest's geometry.

import android.content.Context;
import android.graphics.Bitmap;
import android.graphics.BitmapFactory;
import android.graphics.Canvas;
import android.graphics.Paint;
import android.graphics.Rect;
import android.os.Handler;
import android.os.Looper;
import android.util.Log;
import android.view.MotionEvent;
import android.view.View;
import android.webkit.JavascriptInterface;
import android.webkit.ValueCallback;
import android.webkit.WebView;
import android.webkit.WebViewClient;

import java.io.IOException;
import java.util.Map;
import java.util.concurrent.ConcurrentHashMap;
import java.util.concurrent.atomic.AtomicInteger;

import org.json.JSONArray;
import org.json.JSONObject;

/** The behaviour behind one framework `WebView` instance. */
public final class WebViewChromiumCore {
    static final String TAG = "ACOSWvProvider";

    private static final AtomicInteger NEXT_VIEW = new AtomicInteger();

    private final AcosWebViewTransport.Transport mTransport;
    private final int mView = NEXT_VIEW.incrementAndGet();
    private final Handler mUi = new Handler(Looper.getMainLooper());
    private final Paint mPaint = new Paint(Paint.FILTER_BITMAP_FLAG);
    private final Map<String, JavascriptInterfaceBridge> mInterfaces = new ConcurrentHashMap<>();

    private volatile WebView mWebView;
    private volatile WebViewClient mClient;
    private volatile Bitmap mFrame;
    private volatile int mWidth;
    private volatile int mHeight;
    private volatile float mDensity = 1f;
    private volatile String mUrl = "about:blank";
    private volatile boolean mCreated;
    private volatile boolean mPaused;
    private volatile int mScrollX;
    private volatile int mScrollY;
    private volatile boolean mNetworkAvailable = true;
    private volatile boolean mAttached = true;

    public WebViewChromiumCore(AcosWebViewTransport.Transport transport) {
        mTransport = transport;
        transport.setEventHandler(new Events());
    }

    /** `WebViewFactoryProvider.createWebView(WebView, PrivateAccess)`. */
    void attach(WebView webView) {
        mWebView = webView;
        mDensity = webView.getResources().getDisplayMetrics().density;
        mWidth = webView.getWidth();
        mHeight = webView.getHeight();
        if (mWidth > 0 && mHeight > 0) {
            ensureCreated();
        }
    }

    WebView webView() {
        return mWebView;
    }

    void setWebViewClient(WebViewClient client) {
        mClient = client;
    }

    WebViewClient client() {
        return mClient;
    }

    // -- view lifecycle -----------------------------------------------------

    private void ensureCreated() {
        if (mCreated || mWidth <= 0 || mHeight <= 0) {
            return;
        }
        mCreated = true;
        JSONObject params = params();
        try {
            params.put("view", mView);
            params.put("width", mWidth);
            params.put("height", mHeight);
            params.put("density", (double) mDensity);
            mTransport.call("view.create", params);
        } catch (Exception e) {
            throw failure("view.create", e);
        }
    }

    /** `ViewDelegate.onSizeChanged` -> the guest's viewport (`WIRE.md` §4.2). */
    void onSizeChanged(int w, int h, int oldWidth, int oldHeight) {
        if (w <= 0 || h <= 0) {
            return;
        }
        mWidth = w;
        mHeight = h;
        if (!mCreated) {
            ensureCreated();
            return;
        }
        JSONObject params = params();
        try {
            params.put("view", mView);
            params.put("width", w);
            params.put("height", h);
            params.put("density", (double) mDensity);
            mTransport.call("view.setViewport", params);
        } catch (Exception e) {
            Log.w(TAG, "setViewport failed: " + e);
        }
    }

    void destroy() {
        mCreated = false;
        mFrame = null;
        try {
            mTransport.call("view.destroy", params().put("view", mView));
        } catch (Exception e) {
            Log.w(TAG, "view.destroy failed: " + e);
        }
    }

    // -- navigation ---------------------------------------------------------

    void loadUrl(String url) {
        ensureCreated();
        JSONObject params = params();
        try {
            params.put("view", mView);
            params.put("url", url);
            mTransport.call("view.loadUrl", params);
            mUrl = url;
        } catch (Exception e) {
            if (mClient != null) {
                mUi.post(() -> mClient.onReceivedError(mWebView, -2, e.toString(), url));
            }
        }
    }

    void loadData(String data, String mime, String encoding) {
        ensureCreated();
        JSONObject params = params();
        try {
            params.put("view", mView);
            params.put("data", data == null ? "" : data);
            params.put("mime", mime == null ? "text/html" : mime);
            params.put("encoding", "base64".equalsIgnoreCase(encoding) ? "base64" : "utf8");
            JSONObject result = mTransport.call("view.loadData", params).result();
            mUrl = result == null ? mUrl : result.optString("url", mUrl);
        } catch (Exception e) {
            if (mClient != null) {
                mUi.post(() -> mClient.onReceivedError(mWebView, -2, e.toString(), mUrl));
            }
        }
    }

    void loadDataWithBaseUrl(String baseUrl, String data, String mime, String encoding, String historyUrl) {
        ensureCreated();
        JSONObject params = params();
        try {
            params.put("view", mView);
            params.put("data", data == null ? "" : data);
            params.put("mime", mime == null ? "text/html" : mime);
            params.put("encoding", "base64".equalsIgnoreCase(encoding) ? "base64" : "utf8");
            if (baseUrl != null) {
                params.put("baseUrl", baseUrl);
            }
            JSONObject result = mTransport.call("view.loadData", params).result();
            mUrl = result == null ? mUrl : result.optString("url", mUrl);
        } catch (Exception e) {
            if (mClient != null) {
                mUi.post(() -> mClient.onReceivedError(mWebView, -2, e.toString(), mUrl));
            }
        }
    }

    void setNetworkAvailable(boolean available) {
        // The guest's offline profile is the sandbox's, not a page setting; the
        // value is kept so `NetInfo`-style queries answer honestly, and the
        // broker is told so the engine does not report a connected network.
        mNetworkAvailable = available;
        if (available) {
            return;
        }
        JSONObject params = params();
        try {
            params.put("view", mView);
            params.put("offline", true);
            mTransport.call("view.setNetworkAvailable", params);
        } catch (Exception e) {
            Log.w(TAG, "view.setNetworkAvailable failed: " + e);
        }
    }

    void attachToWindow() {
        mAttached = true;
        ensureCreated();
    }

    void detachFromWindow() {
        mAttached = false;
    }

    void reload() {
        call("view.reload");
    }

    void stopLoading() {
        call("view.stop");
    }

    // -- JavaScript ---------------------------------------------------------

    void evaluateJavascript(String script, ValueCallback<String> callback) {
        JSONObject params = params();
        try {
            params.put("view", mView);
            params.put("script", script);
            JSONObject result = mTransport.call("view.evaluateJavascript", params).result();
            final String value = result == null ? null : result.optString("valueJson", null);
            if (callback != null) {
                mUi.post(() -> callback.onReceiveValue(value));
            }
        } catch (Exception e) {
            if (callback != null) {
                mUi.post(() -> callback.onReceiveValue(null));
            }
        }
    }

    void addJavascriptInterface(Object object, String name) {
        if (name == null || object == null) {
            throw new NullPointerException("name and object must not be null");
        }
        // Android exposes **only** `@JavascriptInterface`-annotated methods; the
        // broker enforces the same list (WIRE.md §5.3), so the security rule is
        // decided here, from this process's own reflection, not by the host.
        JavascriptInterfaceBridge bridge = new JavascriptInterfaceBridge(object);
        mInterfaces.put(name, bridge);
        JSONObject params = params();
        try {
            params.put("view", mView);
            params.put("name", name);
            params.put("methods", new JSONArray(bridge.names()));
            mTransport.call("view.addJavascriptInterface", params);
        } catch (Exception e) {
            throw failure("view.addJavascriptInterface", e);
        }
    }

    void removeJavascriptInterface(String name) {
        mInterfaces.remove(name);
        JSONObject params = params();
        try {
            params.put("view", mView);
            params.put("name", name);
            mTransport.call("view.removeJavascriptInterface", params);
        } catch (Exception e) {
            Log.w(TAG, "removeJavascriptInterface: " + e);
        }
    }

    /** The page-side call table, resolved on this side at call time. */
    private final class JavascriptInterfaceBridge {
        private final Object target;
        private final Map<String, java.lang.reflect.Method> methods = new ConcurrentHashMap<>();

        JavascriptInterfaceBridge(Object target) {
            this.target = target;
            for (java.lang.reflect.Method m : target.getClass().getMethods()) {
                if (m.getAnnotation(JavascriptInterface.class) != null) {
                    methods.put(m.getName(), m);
                }
            }
        }

        java.util.List<String> names() {
            return new java.util.ArrayList<>(methods.keySet());
        }

        String invoke(String method, String argsJson) throws Exception {
            java.lang.reflect.Method m = methods.get(method);
            if (m == null) {
                throw new NoSuchMethodException(
                        "not exposed to JavaScript (no @JavascriptInterface): " + method);
            }
            JSONArray args = argsJson == null ? new JSONArray() : new JSONArray(argsJson);
            Class<?>[] types = m.getParameterTypes();
            Object[] values = new Object[types.length];
            for (int i = 0; i < types.length; i++) {
                Object raw = args.opt(i);
                values[i] = coerce(types[i], raw);
            }
            return String.valueOf(m.invoke(target, values));
        }

        private Object coerce(Class<?> type, Object raw) {
            if (raw == null || raw == JSONObject.NULL) {
                return type.isPrimitive() ? 0 : null;
            }
            if (type == String.class) {
                return String.valueOf(raw);
            }
            if (type == int.class || type == Integer.class) {
                return ((Number) raw).intValue();
            }
            if (type == long.class || type == Long.class) {
                return ((Number) raw).longValue();
            }
            if (type == float.class || type == Float.class) {
                return ((Number) raw).floatValue();
            }
            if (type == double.class || type == Double.class) {
                return ((Number) raw).doubleValue();
            }
            if (type == boolean.class || type == Boolean.class) {
                return (Boolean) raw;
            }
            return raw;
        }
    }

    // -- painting -----------------------------------------------------------

    void setFrame(byte[] png) {
        Bitmap decoded = BitmapFactory.decodeByteArray(png, 0, png.length);
        if (decoded == null) {
            Log.w(TAG, "frame did not decode as an image (" + png.length + " bytes)");
            return;
        }
        mFrame = decoded;
        View view = mWebView;
        if (view != null) {
            view.postInvalidateOnAnimation();
        }
    }

    /** `ViewDelegate.onDraw`: the last frame at the View's size, letterboxed. */
    void onDraw(Canvas canvas) {
        Bitmap frame = mFrame;
        if (frame == null) {
            return;
        }
        int w = mWebView == null ? mWidth : mWebView.getWidth();
        int h = mWebView == null ? mHeight : mWebView.getHeight();
        if (w <= 0 || h <= 0) {
            return;
        }
        Rect src = new Rect(0, 0, frame.getWidth(), frame.getHeight());
        Rect dst = new Rect(0, 0, w, h);
        canvas.drawBitmap(frame, src, dst, mPaint);
    }

    // -- input --------------------------------------------------------------

    /** `ViewDelegate.onTouchEvent` -> `input.touch` (`WIRE.md` §4.2). */
    boolean onTouchEvent(MotionEvent event) {
        JSONObject params = params();
        try {
            params.put("view", mView);
            params.put("action", actionName(event.getActionMasked()));
            JSONArray points = new JSONArray();
            for (int i = 0; i < event.getPointerCount(); i++) {
                JSONObject point = new JSONObject();
                point.put("id", event.getPointerId(i));
                point.put("x", (double) event.getX(i));
                point.put("y", (double) event.getY(i));
                point.put("force", (double) event.getPressure(i));
                points.put(point);
            }
            params.put("points", points);
            mTransport.call("input.touch", params);
        } catch (Exception e) {
            Log.w(TAG, "input.touch failed: " + e);
        }
        return true;
    }

    private static String actionName(int action) {
        switch (action) {
            case MotionEvent.ACTION_DOWN:
                return "down";
            case MotionEvent.ACTION_MOVE:
                return "move";
            case MotionEvent.ACTION_UP:
                return "up";
            default:
                return "cancel";
        }
    }

    void onScrolled(int x, int y) {
        mScrollX = x;
        mScrollY = y;
    }

    int scrollX() {
        return mScrollX;
    }

    int scrollY() {
        return mScrollY;
    }

    // -- host -> guest ------------------------------------------------------

    private final class Events implements AcosWebViewTransport.EventHandler {
        @Override
        public void onEvent(JSONObject event) {
            String name = event.optString("event", "");
            switch (name) {
                case "navigation.started": {
                    final String url = event.optString("url", mUrl);
                    mUrl = url;
                    final WebViewClient client = mClient;
                    if (client != null) {
                        mUi.post(() -> client.onPageStarted(mWebView, url, null));
                    }
                    break;
                }
                case "navigation.finished": {
                    final String url = event.optString("url", mUrl);
                    mUrl = url;
                    final WebViewClient client = mClient;
                    if (client != null) {
                        mUi.post(() -> client.onPageFinished(mWebView, url));
                    }
                    break;
                }
                case "navigation.failed": {
                    final String url = event.optString("url", mUrl);
                    final String message = event.optString("message", "navigation failed");
                    final WebViewClient client = mClient;
                    if (client != null) {
                        mUi.post(() -> client.onReceivedError(mWebView, -2, message, url));
                    }
                    break;
                }
                case "render.invalidated": {
                    if (mPaused) {
                        break;
                    }
                    // Coalesced by the broker: one pull per invalidation, never
                    // one per pixel (`WIRE.md` §5.1).
                    Thread frame = new Thread(() -> {
                        try {
                            AcosWebViewTransport.Frame f = mTransport.render(mView);
                            if (f.blob != null) {
                                setFrame(f.blob);
                            }
                        } catch (IOException e) {
                            Log.w(TAG, "render failed: " + e);
                        }
                    }, "acos-webview-frame");
                    frame.setDaemon(true);
                    frame.start();
                    break;
                }
                default:
                    // Unknown events are ignored (WIRE.md §2, forward compatible).
                    break;
            }
        }

        @Override
        public JSONObject onRequest(String op, JSONObject params) throws AcosWebViewTransport.TransportException {
            if ("nav.shouldOverride".equals(op)) {
                WebViewClient client = mClient;
                String url = params.optString("url", "");
                boolean allow = true;
                if (client != null) {
                    final boolean[] answer = new boolean[1];
                    final java.util.concurrent.CountDownLatch done =
                            new java.util.concurrent.CountDownLatch(1);
                    mUi.post(() -> {
                        answer[0] = (mWebView != null && client.shouldOverrideUrlLoading(mWebView, url));
                        done.countDown();
                    });
                    try {
                        done.await(25, java.util.concurrent.TimeUnit.SECONDS);
                    } catch (InterruptedException e) {
                        Thread.currentThread().interrupt();
                        throw new AcosWebViewTransport.TransportException(
                                "TIMEOUT", "shouldOverrideUrlLoading did not answer");
                    }
                    // `shouldOverrideUrlLoading` returning true means the *app*
                    // handles the URL, i.e. the engine must not navigate.
                    allow = !answer[0];
                }
                JSONObject result = new JSONObject();
                try {
                    result.put("allow", allow);
                } catch (org.json.JSONException e) {
                    throw new AcosWebViewTransport.TransportException("INTERNAL", e.toString());
                }
                return result;
            }
            if ("js.call".equals(op)) {
                String name = params.optString("name", "");
                JavascriptInterfaceBridge bridge = mInterfaces.get(name);
                JSONObject result = new JSONObject();
                try {
                    if (bridge == null) {
                        result.put("resultJson", JSONObject.NULL);
                        result.put("error", "no such interface: " + name);
                        return result;
                    }
                    String value = bridge.invoke(
                            params.optString("method", ""), params.optString("argsJson", "[]"));
                    result.put("resultJson", JSONObject.quote(value));
                    return result;
                } catch (Exception e) {
                    try {
                        result.put("error", e.toString());
                    } catch (org.json.JSONException ignored) {
                    }
                    return result;
                }
            }
            throw new AcosWebViewTransport.TransportException(
                    "UNSUPPORTED", "this provider does not serve host request " + op);
        }
    }

    // -- helpers ------------------------------------------------------------

    private JSONObject params() {
        return new JSONObject();
    }

    private void call(String op) {
        JSONObject params = params();
        try {
            params.put("view", mView);
            mTransport.call(op, params);
        } catch (Exception e) {
            Log.w(TAG, op + " failed: " + e);
        }
    }

    private static RuntimeException failure(String op, Exception e) {
        return e instanceof RuntimeException
                ? (RuntimeException) e
                : new RuntimeException("ACOS WebView provider: " + op + " failed: " + e, e);
    }

    /** The view id the broker knows this View by (diagnostics only). */
    int viewId() {
        return mView;
    }

    /** The guest's density, i.e. what `view.setViewport` reported. */
    float density() {
        return mDensity;
    }

    String currentUrl() {
        return mUrl;
    }

    void setPaused(boolean paused) {
        mPaused = paused;
    }
}
