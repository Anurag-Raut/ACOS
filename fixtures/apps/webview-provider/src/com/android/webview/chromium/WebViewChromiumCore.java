package com.android.webview.chromium;

// WV2: the provider's real code. The generated `WebViewChromium` /
// `WebViewChromiumViewDelegate` / `WebViewChromiumScrollDelegate` /
// `WebSettingsChromium` classes only forward their framework-interface methods
// here, so the framework surface and the behaviour stay apart: the generator
// owns the interface contracts, this class owns what a WebView actually does.
//
// Everything the engine owns is a wire call (results/handoff/WVB/WIRE.md, §4):
// there is no in-process engine, no DevTools, no second process — the host
// broker owns Chromium and this class is a client of it.
//
// WV5a (wall 9): the framework's `new WebView(context)` reaches the provider
// through `View.<init>` -> `WebView.setOverScrollMode` ->
// `ViewDelegate.setOverScrollMode`, and then through the first measure, layout
// and draw. That walk is split by one rule:
//
//   * **guest-only state stays in the guest.** `overScrollMode`, the layer
//     type, the background colour, the scrollbar style, the layout params, the
//     scroll offsets and the `.getSettings()` object are the View's own state;
//     Chromium's `AwContents` keeps the same values in `AwContents` and never
//     sends them anywhere. Keeping them here is the same split, not a bypass.
//   * **anything the engine owns is a WIRE v1 request**: the viewport
//     (`view.create` on the first real measurement, `view.setViewport` on every
//     layout change), navigation (`view.loadUrl`), focus (`input.setFocus`),
//     frames (`render`, pulled once per invalidation from `onDraw`).
//   * **anything still unsupported throws** a structured
//     `UnsupportedOperationException` naming the method — never a silent no-op
//     that reports success. The gaps that would need a new wire message are
//     written up in results/handoff/WV5a/WIRE-ADDITIONS.md.

import android.content.Context;
import android.content.res.Configuration;
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
import android.view.ViewGroup;
import android.webkit.JavascriptInterface;
import android.webkit.ValueCallback;
import android.webkit.WebChromeClient;
import android.webkit.WebSettings;
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
    private final Paint mBackgroundPaint = new Paint();
    private final Map<String, JavascriptInterfaceBridge> mInterfaces = new ConcurrentHashMap<>();

    private volatile WebView mWebView;
    private volatile WebViewClient mClient;
    private volatile WebChromeClient mChromeClient;
    private volatile WebSettingsChromium mSettings;
    private volatile Bitmap mFrame;
    private volatile long mFrameSeq;
    private volatile int mWidth;
    private volatile int mHeight;
    private volatile float mDensity = 1f;
    private volatile String mUrl = "about:blank";
    private volatile String mTitle;
    private volatile boolean mCreated;
    private volatile boolean mPaused;
    private volatile int mScrollX;
    private volatile int mScrollY;
    private volatile boolean mNetworkAvailable = true;
    private volatile boolean mAttached = true;
    /** The layout-confirmed size, i.e. what `view.setViewport` last reported. */
    private volatile int mLayoutWidth;
    private volatile int mLayoutHeight;
    /** The measured size, i.e. what `view.create` was opened with. */
    private volatile int mMeasuredWidth;
    private volatile int mMeasuredHeight;

    // -- guest-only View state (Chromium keeps the same values in AwContents) --
    private volatile int mOverScrollMode = View.OVER_SCROLL_IF_CONTENT_SCROLLS;
    private volatile int mLayerType = View.LAYER_TYPE_NONE;
    private volatile Paint mLayerPaint;
    private volatile int mBackgroundColor;
    private volatile int mScrollBarStyle;
    private volatile ViewGroup.LayoutParams mLayoutParams;
    private volatile int mVisibility = View.VISIBLE;
    private volatile int mWindowVisibility = View.VISIBLE;
    private volatile boolean mHasWindowFocus;
    private volatile boolean mViewFocused;

    // -- frame pull state (coalesced, `render` per invalidation) -------------
    private final Object mFrameLock = new Object();
    private boolean mFrameDirty = true;
    private boolean mFrameInFlight;
    private int mFrameRequests;
    private final AtomicInteger mDraws = new AtomicInteger();

    // -- settings -----------------------------------------------------------
    private volatile boolean mJavascriptEnabled;
    private volatile String mUserAgent;
    private volatile boolean mPrivateBrowsing;

    public WebViewChromiumCore(AcosWebViewTransport.Transport transport) {
        mTransport = transport;
        transport.setEventHandler(new Events());
    }

    /** `WebViewFactoryProvider.createWebView(WebView, PrivateAccess)`. */
    void attach(WebView webView) {
        mWebView = webView;
        // `WebView` is a `ViewGroup`, so `ViewGroup`'s default `WILL_NOT_DRAW`
        // would skip `onDraw` entirely and the page frame would never be
        // painted. Chromium's provider draws in `onDraw` for the same reason.
        webView.setWillNotDraw(false);
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

    void setWebChromeClient(WebChromeClient client) {
        mChromeClient = client;
    }

    WebChromeClient chromeClient() {
        return mChromeClient;
    }

    /** `WebViewProvider.getSettings`: one `WebSettings` per WebView. */
    WebSettings settings() {
        WebSettingsChromium settings = mSettings;
        if (settings == null) {
            synchronized (this) {
                if (mSettings == null) {
                    mSettings = new WebSettingsChromium(this);
                }
                settings = mSettings;
            }
        }
        return settings;
    }

    /**
     * `WebViewProvider.init(Map, boolean)`: the framework hands the JS
     * interfaces from the hidden constructor over before any other provider
     * call. Private browsing has no wire message (`WIRE-ADDITIONS.md` §3), so
     * asking for it is an honest refusal instead of a shared-cache WebView that
     * silently is not private.
     */
    void init(Map<String, Object> javaScriptInterfaces, boolean privateBrowsing) {
        if (privateBrowsing) {
            throw new UnsupportedOperationException(
                    "ACOS WebView provider: unsupported feature privateBrowsing "
                            + "(WebViewProvider.init(Ljava/util/Map;Z)V with privateBrowsing=true; "
                            + "WIRE v1 has no per-view profile, see results/handoff/WV5a/WIRE-ADDITIONS.md)");
        }
        mPrivateBrowsing = false;
        // `WebView` is a `ViewGroup`, and `ViewGroup`'s constructor sets
        // `WILL_NOT_DRAW`, which makes `View.draw` skip `onDraw` — the page frame
        // would never be painted and `onDraw` would never ask for one. This is
        // the last provider call of the framework constructor (`ensureProviderCreated`
        // -> `init`), i.e. after `ViewGroup`'s constructor has run, so the flag
        // is cleared here for good. Chromium's provider does the same in `init`.
        WebView view = mWebView;
        if (view != null) {
            view.setWillNotDraw(false);
            // The engine paints its own scrollbars; the guest has none to offer
            // (`WIRE-ADDITIONS.md` §4 — WIRE v1 has no content size).
            view.setVerticalScrollBarEnabled(false);
            view.setHorizontalScrollBarEnabled(false);
        }
        if (javaScriptInterfaces != null) {
            for (Map.Entry<String, Object> entry : javaScriptInterfaces.entrySet()) {
                addJavascriptInterface(entry.getValue(), entry.getKey());
            }
        }
    }

    // -- guest-only View state ----------------------------------------------

    /**
     * `ViewDelegate.setOverScrollMode`: the overscroll glow is the View's, not
     * the page's. Chromium keeps `AwContents.mOverScrollMode` for the same
     * reason; there is no engine op and none is needed.
     */
    void setOverScrollMode(int mode) {
        mOverScrollMode = mode;
    }

    int overScrollMode() {
        return mOverScrollMode;
    }

    /** `ViewDelegate.setLayerType`: which layer the *guest* Canvas renders into. */
    void setLayerType(int layerType, Paint paint) {
        mLayerType = layerType;
        mLayerPaint = paint;
    }

    int layerType() {
        return mLayerType;
    }

    Paint layerPaint() {
        return mLayerPaint;
    }

    void setBackgroundColor(int color) {
        mBackgroundColor = color;
        mBackgroundPaint.setColor(color);
        invalidate();
    }

    int backgroundColor() {
        return mBackgroundColor;
    }

    void setScrollBarStyle(int style) {
        mScrollBarStyle = style;
    }

    int scrollBarStyle() {
        return mScrollBarStyle;
    }

    /**
     * `ViewDelegate.setLayoutParams`. `WebView.setLayoutParams` is a **pure
     * delegate** in this image (the image dex has no `super` call, the same as
     * `setFrame`), so the View-side bookkeeping that `View.setLayoutParams`
     * normally performs — storing `mLayoutParams`, resolving margins and
     * requesting a layout — never happens unless the provider does it. Without
     * it a parent reads `getLayoutParams() == null` and dies in
     * `LinearLayout.measureVertical` (measured: `LOOPER_THREW`,
     * `NPE ... LinearLayout$LayoutParams.weight on a null object reference`).
     *
     * <p>The provider is the View's own setter here, so it performs exactly
     * those three steps: the field (the only route left, `setLayoutParams` being
     * the override itself), then `requestLayout()`.
     */
    void setLayoutParams(ViewGroup.LayoutParams params) {
        mLayoutParams = params;
        WebView view = mWebView;
        if (view == null) {
            return;
        }
        try {
            layoutParamsField().set(view, params);
        } catch (Throwable e) {
            // Never silent: a WebView whose parent cannot see its layout params
            // is broken, and the app must hear about it.
            throw new IllegalStateException(
                    "ACOS WebView provider: could not install the View's layout params "
                            + "(View.mLayoutParams): " + e, e);
        }
        view.requestLayout();
    }

    private static java.lang.reflect.Field sLayoutParamsField;

    private static java.lang.reflect.Field layoutParamsField() throws NoSuchFieldException {
        java.lang.reflect.Field field = sLayoutParamsField;
        if (field == null) {
            field = android.view.View.class.getDeclaredField("mLayoutParams");
            field.setAccessible(true);
            sLayoutParamsField = field;
        }
        return field;
    }

    ViewGroup.LayoutParams layoutParams() {
        return mLayoutParams;
    }

    private void invalidate() {
        WebView view = mWebView;
        if (view != null) {
            view.postInvalidateOnAnimation();
        }
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
            Log.i(TAG, "view.create view=" + mView + " " + mWidth + "x" + mHeight
                    + " density=" + mDensity);
        } catch (Exception e) {
            throw failure("view.create", e);
        }
    }

    /**
     * `ViewDelegate.onMeasure`: the framework has told us the size the View will
     * have (`View.onMeasure` runs its own default measurement *before*
     * forwarding here, so `getMeasuredWidth/Height` are already correct). That
     * is the first moment a real geometry exists, so it is where the engine view
     * is created (`view.create`, `WIRE.md` §4.2).
     */
    void onMeasure(int widthMeasureSpec, int heightMeasureSpec) {
        int w = measureSize(widthMeasureSpec, mMeasuredWidth);
        int h = measureSize(heightMeasureSpec, mMeasuredHeight);
        mMeasuredWidth = w;
        mMeasuredHeight = h;
        if (w <= 0 || h <= 0) {
            return;
        }
        if (!mCreated) {
            mWidth = w;
            mHeight = h;
            ensureCreated();
        }
    }

    /**
     * `View.MeasureSpec` -> pixels, the same rule `View.getDefaultSize` uses
     * (the framework already measured the View with it): a bounded spec is the
     * size, an UNSPECIFIED one leaves the previous size.
     */
    private static int measureSize(int measureSpec, int previous) {
        int mode = View.MeasureSpec.getMode(measureSpec);
        int size = View.MeasureSpec.getSize(measureSpec);
        if (mode == View.MeasureSpec.UNSPECIFIED) {
            return previous;
        }
        return size;
    }

    /**
     * `ViewDelegate.setFrame`. `WebView.setFrame` is a full override (the
     * framework does not call `View.setFrame` for a WebView), so the provider
     * owns the frame bookkeeping: `setLeft/Top/Right/Bottom` are the public
     * `View` API that sets exactly the fields `View.setFrame` sets, and the
     * size change goes to the engine as `view.setViewport` (`WIRE.md` §4.2).
     *
     * <p>Layout is authoritative: `view.create` was opened with the *measured*
     * size, and the layout size is then pushed as a real resize, so the engine
     * viewport is never a guess. A repeated layout at the same size sends
     * nothing.
     */
    boolean setFrame(int left, int top, int right, int bottom) {
        WebView view = mWebView;
        int w = right - left;
        int h = bottom - top;
        boolean changed = w != mWidth || h != mHeight;
        if (view != null) {
            view.setLeft(left);
            view.setTop(top);
            view.setRight(right);
            view.setBottom(bottom);
        }
        mWidth = w;
        mHeight = h;
        if (w <= 0 || h <= 0) {
            return changed;
        }
        if (!mCreated) {
            ensureCreated();
            return changed;
        }
        if (w != mLayoutWidth || h != mLayoutHeight) {
            mLayoutWidth = w;
            mLayoutHeight = h;
            sendViewport(w, h);
        }
        return changed;
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
        if (w == mLayoutWidth && h == mLayoutHeight) {
            return;
        }
        mLayoutWidth = w;
        mLayoutHeight = h;
        sendViewport(w, h);
    }

    private void sendViewport(int w, int h) {
        JSONObject params = params();
        try {
            params.put("view", mView);
            params.put("width", w);
            params.put("height", h);
            params.put("density", (double) mDensity);
            mTransport.call("view.setViewport", params);
            Log.i(TAG, "view.setViewport view=" + mView + " " + w + "x" + h
                    + " density=" + mDensity);
        } catch (Exception e) {
            Log.w(TAG, "view.setViewport failed: " + e);
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

    // -- visibility and focus (both engine state, both on the wire) ---------

    /**
     * `ViewDelegate.onVisibilityChanged`. The View's visibility is guest state;
     * what it *means* for the engine (renderer scheduling, `Page.setLifecycle
     * Events`) has no WIRE v1 message yet, so the guest only stops asking for
     * frames while it is not visible and the gap is written up in
     * `WIRE-ADDITIONS.md` §1 — nothing claims the engine was told.
     */
    void onVisibilityChanged(View changedView, int visibility) {
        mVisibility = visibility;
        if (visibility == View.VISIBLE) {
            mFrameDirty = true;
            invalidate();
        }
    }

    void onWindowVisibilityChanged(int visibility) {
        mWindowVisibility = visibility;
        if (visibility == View.VISIBLE) {
            mFrameDirty = true;
            invalidate();
        }
    }

    /** The frame pull is pointless while the View cannot be seen. */
    private boolean renderable() {
        return mAttached && mCreated && mVisibility == View.VISIBLE
                && mWindowVisibility == View.VISIBLE;
    }

    /**
     * `ViewDelegate.onWindowFocusChanged`: window focus is engine state — it is
     * what turns the page's `document.hasFocus()` on and starts the renderer's
     * focus scheduling — so it goes over the wire as `input.setFocus`
     * (`WIRE.md` §4.2), combined with the View's own focus.
     */
    void onWindowFocusChanged(boolean hasWindowFocus) {
        mHasWindowFocus = hasWindowFocus;
        sendFocus();
    }

    /** `ViewDelegate.onFocusChanged`: the same op, from the View's focus. */
    void onFocusChanged(boolean focused, int direction, Rect previouslyFocusedRect) {
        mViewFocused = focused;
        sendFocus();
    }

    /** `ViewDelegate.requestFocus`: the View takes focus, so the page gets it. */
    boolean requestFocus(int direction, Rect previouslyFocusedRect) {
        mViewFocused = true;
        sendFocus();
        return true;
    }

    private void sendFocus() {
        boolean focused = mViewFocused && mHasWindowFocus;
        JSONObject params = params();
        try {
            params.put("view", mView);
            params.put("focused", focused);
            mTransport.call("input.setFocus", params);
            Log.i(TAG, "input.setFocus view=" + mView + " focused=" + focused);
        } catch (Exception e) {
            Log.w(TAG, "input.setFocus failed: " + e);
        }
    }

    void attachToWindow() {
        mAttached = true;
        ensureCreated();
    }

    void detachFromWindow() {
        mAttached = false;
    }

    // -- navigation ---------------------------------------------------------

    void loadUrl(String url) {
        ensureCreated();
        JSONObject params = params();
        try {
            params.put("view", mView);
            params.put("url", url);
            Log.i(TAG, "view.loadUrl view=" + mView + " url=" + url);
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

    void reload() {
        call("view.reload");
    }

    void stopLoading() {
        call("view.stop");
    }

    // -- settings -----------------------------------------------------------

    void setJavascriptEnabled(boolean enabled) {
        if (mCreated && enabled != mJavascriptEnabled) {
            // `view.create` is the only WIRE v1 message that carries this flag
            // (`WIRE.md` §4.2); changing it afterwards has no wire message, and
            // pretending it took effect would be a false success.
            throw new UnsupportedOperationException(
                    "ACOS WebView provider: unsupported method WebSettings.setJavaScriptEnabled(Z) "
                            + "after the engine view was created (WIRE v1 has no view.setSettings; "
                            + "see results/handoff/WV5a/WIRE-ADDITIONS.md)");
        }
        mJavascriptEnabled = enabled;
    }

    boolean javascriptEnabled() {
        return mJavascriptEnabled;
    }

    /** `WebSettings.setUserAgentString` -> `view.setUserAgent` (`WIRE.md` §4.2). */
    void setUserAgent(String userAgent) {
        mUserAgent = userAgent;
        if (!mCreated) {
            return;
        }
        JSONObject params = params();
        try {
            params.put("view", mView);
            params.put("userAgent", userAgent);
            mTransport.call("view.setUserAgent", params);
        } catch (Exception e) {
            throw failure("view.setUserAgent", e);
        }
    }

    String userAgent() {
        return mUserAgent;
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

    void setFrame(byte[] png, long seq) {
        Bitmap decoded = BitmapFactory.decodeByteArray(png, 0, png.length);
        if (decoded == null) {
            Log.w(TAG, "frame did not decode as an image (" + png.length + " bytes)");
            return;
        }
        mFrame = decoded;
        mFrameSeq = seq;
        mFrameDirty = false;
        Log.i(TAG, "frame decoded " + decoded.getWidth() + "x" + decoded.getHeight()
                + " seq=" + seq + " bytes=" + png.length);
        invalidate();
    }

    /**
     * `ViewDelegate.onDraw`: paint the last frame at the View's own size
     * (letterboxed into it, `FILTER_BITMAP`) and, per `WIRE.md` §4.2, ask the
     * engine for the next one when something invalidated it. The pull is
     * coalesced — at most one `render` in flight, never one per pixel.
     */
    void onDraw(Canvas canvas) {
        WebView view = mWebView;
        int w = view == null ? mWidth : view.getWidth();
        int h = view == null ? mHeight : view.getHeight();
        if (w <= 0 || h <= 0) {
            return;
        }
        if (mDraws.getAndIncrement() < 3) {
            Log.i(TAG, "onDraw " + w + "x" + h + " dirty=" + mFrameDirty
                    + " frame=" + (mFrame == null ? "none" : "present")
                    + " renderable=" + renderable());
        }
        if (mBackgroundColor != 0) {
            canvas.drawRect(0, 0, w, h, mBackgroundPaint);
        }
        Bitmap frame = mFrame;
        if (frame != null) {
            Rect src = new Rect(0, 0, frame.getWidth(), frame.getHeight());
            Rect dst = new Rect(0, 0, w, h);
            canvas.drawBitmap(frame, src, dst, mPaint);
        }
        requestFrame();
    }

    /** One `render` per invalidation, and only while the View can be seen. */
    private void requestFrame() {
        if (!renderable() || !mFrameDirty) {
            return;
        }
        synchronized (mFrameLock) {
            if (mFrameInFlight || !mFrameDirty) {
                return;
            }
            mFrameInFlight = true;
        }
        mFrameRequests++;
        Log.i(TAG, "onDraw " + mWidth + "x" + mHeight + " frame="
                + (mFrame == null ? "none" : mFrame.getWidth() + "x" + mFrame.getHeight())
                + " -> render #" + mFrameRequests);
        Thread pull = new Thread(() -> {
            try {
                AcosWebViewTransport.Frame f = mTransport.render(mView);
                JSONObject result = f.result();
                long seq = result == null ? 0 : result.optLong("seq", 0);
                if (f.blob != null && f.blob.length > 0) {
                    setFrame(f.blob, seq);
                } else {
                    Log.w(TAG, "render returned no frame data");
                }
            } catch (IOException e) {
                Log.w(TAG, "render failed: " + e);
            } finally {
                synchronized (mFrameLock) {
                    mFrameInFlight = false;
                }
            }
        }, "acos-webview-frame");
        pull.setDaemon(true);
        pull.start();
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

    /**
     * `ScrollDelegate.computeScroll`: the guest's own scroller has nothing to
     * advance (a WebView's scrolling is the engine's, and `flingScroll` is still
     * unsupported), so there is nothing to compute here. Reported in
     * `WIRE-ADDITIONS.md` §4: a real engine scroll needs a scroll-position
     * message, which WIRE v1 does not have.
     */
    void computeScroll() {
        // Nothing to advance: no guest scroller, no engine scroll request.
    }

    /**
     * `ScrollDelegate.compute*ScrollRange/Offset/Extent`.
     *
     * <p>WIRE v1 carries no content size and no engine scroll offset
     * (`WIRE-ADDITIONS.md` §4), so the only truthful answer is the guest's own
     * geometry: the View is as big as its viewport, at the offset the guest has
     * scrolled to (0 unless the app called `scrollTo`, which the engine does not
     * follow yet). Reporting a fabricated content height would be worse: it
     * would draw a scrollbar for a range nobody measured. The guest's own
     * scrollbars are off (`init`), because the engine draws its own.
     */
    int computeHorizontalScrollRange() {
        return mWidth;
    }

    int computeHorizontalScrollOffset() {
        return mScrollX;
    }

    int computeVerticalScrollRange() {
        return mHeight;
    }

    int computeVerticalScrollOffset() {
        return mScrollY;
    }

    int computeVerticalScrollExtent() {
        return mHeight;
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

    /** `WebViewDelegate.onMovedToDisplay` / `onConfigurationChanged`: density. */
    void onConfigurationChanged(Configuration config) {
        updateDensity(config);
    }

    void onMovedToDisplay(int displayId, Configuration config) {
        updateDensity(config);
    }

    private void updateDensity(Configuration config) {
        WebView view = mWebView;
        if (config == null || view == null) {
            return;
        }
        float density = view.getResources().getDisplayMetrics().density;
        if (density == mDensity) {
            return;
        }
        mDensity = density;
        if (!mCreated || mWidth <= 0 || mHeight <= 0) {
            return;
        }
        // A density change is a viewport change (`WIRE.md` §4.2 carries
        // `density`), so it is re-sent with the same pixel size.
        mLayoutWidth = 0;
        sendViewport(mWidth, mHeight);
    }

    /**
     * `ViewDelegate.findFocus`: this View is the focused one when it holds
     * focus, otherwise the framework's own answer stands.
     */
    View findFocus(View superFocus) {
        return mViewFocused && mWebView != null ? mWebView : superFocus;
    }

    /**
     * `ViewDelegate.onCheckIsTextEditor`: the IME asks whether an editable text
     * field owns the focus. WIRE v1 has no message reporting an engine text
     * field, so until `input.text`'s counterpart lands the answer is "no" — the
     * same answer the page gives while no field is focused
     * (`WIRE-ADDITIONS.md` §5).
     */
    boolean isTextEditor() {
        return false;
    }

    /** `ViewDelegate.onCreateInputConnection`: no engine text field ⇒ no IME. */
    android.view.inputmethod.InputConnection onCreateInputConnection(
            android.view.inputmethod.EditorInfo outAttrs) {
        return null;
    }

    /**
     * `ViewDelegate.onInitializeAccessibilityNodeInfo`: what Chromium's
     * `AwContents` sets on the node a WebView exposes as a *View* (the page's
     * own virtual tree is the separate `ax` question, §5).
     */
    void onInitializeAccessibilityNodeInfo(android.view.accessibility.AccessibilityNodeInfo info) {
        if (info == null || mWebView == null) {
            return;
        }
        info.setClassName("android.webkit.WebView");
        info.setPackageName(mWebView.getContext().getPackageName());
        info.setSource(mWebView);
        info.setEnabled(mWebView.isEnabled());
        info.setFocusable(mWebView.isFocusable());
        info.setFocused(mViewFocused);
        info.setScrollable(false);
    }

    /** `ViewDelegate.onInitializeAccessibilityEvent`: the same class identity. */
    void onInitializeAccessibilityEvent(android.view.accessibility.AccessibilityEvent event) {
        if (event == null || mWebView == null) {
            return;
        }
        event.setClassName("android.webkit.WebView");
        event.setPackageName(mWebView.getContext().getPackageName());
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
                    mTitle = event.optString("title", mTitle);
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
                    // Coalesced by the broker: one pull per invalidation, never
                    // one per pixel (`WIRE.md` §5.1). The pull happens in
                    // `onDraw` so it is the View's own draw that asks.
                    mFrameDirty = true;
                    invalidate();
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

    String currentTitle() {
        return mTitle;
    }

    long frameSeq() {
        return mFrameSeq;
    }

    boolean privateBrowsing() {
        return mPrivateBrowsing;
    }

    void setPaused(boolean paused) {
        mPaused = paused;
    }
}
