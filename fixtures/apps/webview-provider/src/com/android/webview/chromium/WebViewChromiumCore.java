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
import android.os.Bundle;
import android.os.Handler;
import android.os.Looper;
import android.text.Editable;
import android.util.Base64;
import android.util.Log;
import android.view.accessibility.AccessibilityNodeInfo;
import android.view.accessibility.AccessibilityNodeProvider;
import android.view.inputmethod.BaseInputConnection;
import android.view.inputmethod.EditorInfo;
import android.view.inputmethod.InputConnection;
import android.view.MotionEvent;
import android.view.View;
import android.view.ViewGroup;
import android.webkit.JavascriptInterface;
import android.webkit.ValueCallback;
import android.webkit.WebChromeClient;
import android.webkit.WebSettings;
import android.webkit.WebView;
import android.webkit.WebViewClient;

import java.io.ByteArrayOutputStream;
import java.io.IOException;
import java.io.InputStream;
import java.util.HashMap;
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
    /** The framework's provider seam (`WebView.PrivateAccess`), as AwContents. */
    private volatile WebView.PrivateAccess mWebViewPrivate;
    private final int mView = NEXT_VIEW.incrementAndGet();
    private final Handler mUi = new Handler(Looper.getMainLooper());
    private final Paint mPaint = new Paint(Paint.FILTER_BITMAP_FLAG);
    private final Paint mBackgroundPaint = new Paint();
    private final java.util.concurrent.atomic.AtomicInteger mFrameDraws =
            new java.util.concurrent.atomic.AtomicInteger();
    private final java.util.concurrent.atomic.AtomicInteger mFrameDrawLogs =
            new java.util.concurrent.atomic.AtomicInteger();
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
    /** v1.1: the engine's own content size / scroll offset, in device pixels. */
    private volatile int mContentWidth;
    private volatile int mContentHeight;
    private volatile int mEngineScrollX;
    private volatile int mEngineScrollY;
    private volatile boolean mHasEngineMetrics;
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
    // A WebView paints white before its first page frame, like Android's.
    private volatile int mBackgroundColor = 0xFFFFFFFF;
    {
        // `Paint`'s default colour is opaque black, so the field's white default
        // has to be pushed into the paint: without this the View painted a black
        // rectangle over everything it drew (the "black WebView region" bug).
        mBackgroundPaint.setColor(mBackgroundColor);
    }
    private volatile int mScrollBarStyle;
    private volatile ViewGroup.LayoutParams mLayoutParams;
    private volatile int mVisibility = View.VISIBLE;
    private volatile int mWindowVisibility = View.VISIBLE;
    // v1.1: the page's focused editable field (view.focusChanged).
    private volatile Boolean mVisibilitySent;
    private volatile boolean mTextEditorFocused;
    private volatile String mFocusedFieldType = "";
    // v1.1: the ax snapshot behind the AccessibilityNodeProvider. One snapshot
    // serves a whole tree traversal; any engine invalidation drops it.
    private final Object mAxLock = new Object();
    private JSONObject mAxSnapshot;
    private boolean mAxStale = true;
    private boolean mAxInFlight;
    /** Set by an input: the next tree read must fetch the post-input tree. */
    private boolean mAxForceSync;
    /** WVFAIR: read sequence, so a *later* read always wins the snapshot.
     * A bounded read can be abandoned by the UI thread (the page is too slow)
     * and finish after a later one; without the sequence the older answer
     * overwrites the newer tree and a node the step just observed disappears. */
    private long mAxReadSeq;
    private long mAxSnapshotSeq;
    private AxProvider mAxProvider;

    private volatile boolean mHasWindowFocus;
    private volatile boolean mViewFocused;

    // -- frame pull state (coalesced, `render` per invalidation) -------------
    private final Object mFrameLock = new Object();
    // Start clean: the first `render` is asked for by the engine's first
    // invalidation (navigation.finished/input), when the page has content. A
    // pull before the first navigation produces no compositor frame at all.
    private boolean mFrameDirty;
    private boolean mFrameInFlight;
    /** An invalidation arrived while the one allowed `render` was in flight. */
    private volatile boolean mFrameDirtyDuringFlight;
    private int mFrameRequests;
    private int mFrameRetries;
    private final AtomicInteger mDraws = new AtomicInteger();

    // -- settings -----------------------------------------------------------
    private volatile boolean mJavascriptEnabled;
    private volatile String mUserAgent;
    private volatile boolean mPrivateBrowsing;
    private volatile int mCacheMode = -1;
    private volatile boolean mLoadsImages = true;
    private volatile boolean mBlockNetworkImage;
    // WV8: the two viewport switches. Android's defaults are `false`: the layout
    // viewport is the View's width in CSS px and `<meta viewport>` is ignored
    // until the app asks for the wide arm.
    private volatile boolean mUseWideViewPort;
    private volatile boolean mLoadWithOverviewMode;

    /**
     * One transport per session, one core per View: events and host requests
     * carry a `view` id (`WIRE.md` §5), so a single router dispatches them to
     * the core that owns that id. Installing the handler per core would leave
     * only the last WebView receiving every event (measured with two WebViews:
     * page A's load callbacks arrived on B).
     */
    private static final Map<Integer, WebViewChromiumCore> sCores = new ConcurrentHashMap<>();

    private static final class Router implements AcosWebViewTransport.EventHandler {
        @Override
        public void onEvent(JSONObject event) {
            WebViewChromiumCore core = sCores.get(event.optInt("view", -1));
            if (core != null) {
                core.mEvents.onEvent(event);
            }
        }

        @Override
        public JSONObject onRequest(String op, JSONObject params)
                throws AcosWebViewTransport.TransportException {
            WebViewChromiumCore core = sCores.get(params.optInt("view", -1));
            if (core == null) {
                throw new AcosWebViewTransport.TransportException(
                        "NO_VIEW", "no WebView registered for " + params.optInt("view", -1));
            }
            return core.mEvents.onRequest(op, params);
        }
    }

    private final Events mEvents = new Events();

    public WebViewChromiumCore(AcosWebViewTransport.Transport transport) {
        mTransport = transport;
        sCores.put(mView, this);
        transport.setEventHandler(new Router());
    }

    /** `WebViewFactoryProvider.createWebView(WebView, PrivateAccess)`. */
    void attach(WebView webView, WebView.PrivateAccess privateAccess) {
        mWebView = webView;
        mWebViewPrivate = privateAccess;
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
     * <p>The provider is the View's own setter here, so it must run the super
     * implementation itself. `WebView.PrivateAccess.super_setLayoutParams` is
     * the seam AOSP gives providers for exactly this — Chromium's
     * `WebViewChromium` calls it through `mWebViewPrivate`, no reflection.
     */
    void setLayoutParams(ViewGroup.LayoutParams params) {
        mLayoutParams = params;
        WebView view = mWebView;
        if (view == null) {
            return;
        }
        WebView.PrivateAccess access = mWebViewPrivate;
        if (access == null) {
            // Never silent: a WebView whose parent cannot see its layout params
            // is broken, and the app must hear about it.
            throw new IllegalStateException(
                    "ACOS WebView provider: no WebView.PrivateAccess for setLayoutParams");
        }
        access.super_setLayoutParams(params);
        view.requestLayout();
    }

    ViewGroup.LayoutParams layoutParams() {
        return mLayoutParams;
    }

    private void invalidate() {
        WebView view = mWebView;
        if (view != null) {
            // `postInvalidateOnAnimation` dispatches through the guest's
            // Choreographer animation callback; `postInvalidate` posts an
            // ordinary UI-thread invalidate. Both must end in the same
            // traversal, and a frame the guest asked for must be painted, so
            // the provider asks for both (idempotent; one draw).
            final WebView target = view;
            view.post(new Runnable() {
                @Override
                public void run() {
                    target.invalidate();
                }
            });
            view.postInvalidateOnAnimation();
            view.postInvalidate();
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
            params.put("javascriptEnabled", mJavascriptEnabled);
            if (mUserAgent != null) {
                params.put("userAgent", mUserAgent);
            }
            mTransport.call("view.create", params);
            Log.i(TAG, "view.create view=" + mView + " " + mWidth + "x" + mHeight
                    + " density=" + mDensity + " js=" + mJavascriptEnabled);
        } catch (Exception e) {
            throw failure("view.create", e);
        }
        // v1.1: the engine must know a View that was already hidden when its
        // view was created (`view.setVisibility`, WIRE.md §4.2).
        sendVisibility();
        // WV8: `settings.setUseWideViewPort(true)` is normally called *before*
        // the first `loadUrl`, i.e. before this provider created the engine view
        // (the first measure). The switch is guest state until then, exactly like
        // `javascriptEnabled`, so the create path replays it here.
        if (mUseWideViewPort) {
            applySetting("useWideViewPort", Boolean.TRUE);
        }
        if (mLoadWithOverviewMode) {
            applySetting("loadWithOverviewMode", Boolean.TRUE);
        }
        // JS interfaces registered before the engine view existed.
        for (Map.Entry<String, JavascriptInterfaceBridge> entry : mInterfaces.entrySet()) {
            sendJavascriptInterface(entry.getKey(), entry.getValue());
        }
        // Warm the accessibility cache off the UI thread.
        requestAxRefresh();
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
        WebView.PrivateAccess access = mWebViewPrivate;
        if (access != null) {
            // The same seam AwContents uses: the provider decides the measured
            // dimension, not a guessed call into the View's own fields.
            access.setMeasuredDimension(w, h);
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
        sCores.remove(mView);
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
        sendVisibility();
    }

    void onWindowVisibilityChanged(int visibility) {
        mWindowVisibility = visibility;
        if (visibility == View.VISIBLE) {
            mFrameDirty = true;
            invalidate();
        }
        sendVisibility();
    }

    /**
     * v1.1 `view.setVisibility`: the engine stops the renderer while the View
     * cannot be seen (WIRE.md §4.2), instead of running a page nobody paints.
     */
    private void sendVisibility() {
        if (!mCreated) {
            return;
        }
        boolean visible = mVisibility == View.VISIBLE && mWindowVisibility == View.VISIBLE;
        Boolean sent = mVisibilitySent;
        if (sent == null) {
            // The engine starts active: the initial visible state needs no
            // message (and sending it would freeze/unfreeze a page for nothing).
            mVisibilitySent = Boolean.valueOf(visible);
            if (visible) {
                return;
            }
        } else if (sent.booleanValue() == visible) {
            return;
        }
        JSONObject params = params();
        try {
            params.put("view", mView);
            params.put("visible", mVisibility == View.VISIBLE);
            params.put("windowVisible", mWindowVisibility == View.VISIBLE);
            mVisibilitySent = Boolean.valueOf(visible);
            mTransport.call("view.setVisibility", params);
            Log.i(TAG, "view.setVisibility view=" + mView + " visible="
                    + (mVisibility == View.VISIBLE) + " windowVisible="
                    + (mWindowVisibility == View.VISIBLE));
        } catch (Exception e) {
            Log.w(TAG, "view.setVisibility failed: " + e);
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

    /**
     * `WebView.loadUrl` is asynchronous on Android: the framework returns
     * immediately and the load happens in the engine. It must be asynchronous
     * here too, because a navigation makes the broker ask this provider
     * `nav.shouldOverride` on the transport thread, and that answer needs the
     * UI thread (`shouldOverrideUrlLoading`). Blocking the UI thread on
     * `view.loadUrl` would deadlock against that question (measured: the asset
     * page's load waited out both timeouts and no frame ever reached C1).
     */
    void loadUrl(String url) {
        ensureCreated();
        mUrl = url;
        JSONObject params = params();
        try {
            params.put("view", mView);
            params.put("url", url);
        } catch (org.json.JSONException e) {
            throw failure("view.loadUrl", e);
        }
        Log.i(TAG, "view.loadUrl view=" + mView + " url=" + url);
        Thread load = new Thread(() -> {
            try {
                mTransport.call("view.loadUrl", params);
                Log.i(TAG, "view.loadUrl done view=" + mView);
            } catch (Exception e) {
                Log.w(TAG, "view.loadUrl failed: " + e);
                if (mClient != null) {
                    mUi.post(() -> mClient.onReceivedError(mWebView, -2, e.toString(), url));
                }
            }
        }, "acos-webview-load");
        load.setDaemon(true);
        load.start();
    }

    void loadData(String data, String mime, String encoding) {
        ensureCreated();
        JSONObject params = params();
        try {
            params.put("view", mView);
            params.put("data", data == null ? "" : data);
            params.put("mime", mime == null ? "text/html" : mime);
            params.put("encoding", "base64".equalsIgnoreCase(encoding) ? "base64" : "utf8");
            final JSONObject sent = params;
            Thread load = new Thread(() -> {
                try {
                    JSONObject result = mTransport.call("view.loadData", sent).result();
                    if (result != null) {
                        mUrl = result.optString("url", mUrl);
                    }
                } catch (Exception e) {
                    Log.w(TAG, "view.loadData failed: " + e);
                }
            }, "acos-webview-loaddata");
            load.setDaemon(true);
            load.start();
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
            final JSONObject sent = params;
            Thread load = new Thread(() -> {
                try {
                    JSONObject result = mTransport.call("view.loadData", sent).result();
                    if (result != null) {
                        mUrl = result.optString("url", mUrl);
                    }
                } catch (Exception e) {
                    Log.w(TAG, "view.loadData failed: " + e);
                }
            }, "acos-webview-loaddata");
            load.setDaemon(true);
            load.start();
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
            // v1.1 `view.settings`: a setting changed after `view.create` is a
            // real engine change or a structured refusal -- never a silent one.
            applySetting("javascriptEnabled", Boolean.valueOf(enabled));
        }
        mJavascriptEnabled = enabled;
    }

    boolean javascriptEnabled() {
        return mJavascriptEnabled;
    }

    /**
     * v1.1 `view.settings` (`WIRE.md` §4.2). The broker applies what the engine
     * can honour and names what it refuses; a refused key is an honest
     * `UnsupportedOperationException`, never a setting the app believes changed.
     */
    private void applySetting(String key, Object value) {
        JSONObject setting = params();
        JSONObject settings = params();
        JSONObject params = params();
        try {
            setting.put(key, value);
            settings.put("settings", setting);
            params.put("view", mView);
            java.util.Iterator<String> keys = settings.keys();
            while (keys.hasNext()) {
                String k = keys.next();
                params.put(k, settings.get(k));
            }
            JSONObject result = mTransport.call("view.settings", params).result();
            JSONArray rejected = result == null ? null : result.optJSONArray("rejected");
            if (rejected != null && rejected.length() > 0) {
                throw new UnsupportedOperationException(
                        "ACOS WebView provider: WebSettings." + key + " is not implemented by the "
                                + "engine (view.settings rejected it; WIRE v1.1 §4.2)");
            }
            Log.i(TAG, "view.settings applied " + key + "=" + value);
        } catch (UnsupportedOperationException e) {
            throw e;
        } catch (Exception e) {
            throw failure("view.settings", e);
        }
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

    /** v1.1 settings the engine can honour through `view.settings`. */
    void setCacheMode(int mode) {
        if (mCreated) {
            applySetting("cacheMode", Integer.valueOf(mode));
        }
        mCacheMode = mode;
    }

    int cacheMode() {
        return mCacheMode;
    }

    void setLoadsImagesAutomatically(boolean enabled) {
        if (mCreated) {
            applySetting("loadsImagesAutomatically", Boolean.valueOf(enabled));
        }
        mLoadsImages = enabled;
    }

    boolean loadsImagesAutomatically() {
        return mLoadsImages;
    }

    void setBlockNetworkImage(boolean block) {
        if (mCreated) {
            applySetting("blockNetworkImage", Boolean.valueOf(block));
        }
        mBlockNetworkImage = block;
    }

    boolean blockNetworkImage() {
        return mBlockNetworkImage;
    }

    /**
     * `WebSettings.setUseWideViewPort` (`WIRE.md` §4.2 `view.settings`): with it
     * on, the engine honours the page's `<meta name="viewport">` (and, without
     * one, lays out at Chromium's 980 CSS px Android default); with it off (the
     * default) the layout width is the View's width in CSS px.
     */
    void setUseWideViewPort(boolean enable) {
        mUseWideViewPort = enable;
        if (mCreated) {
            applySetting("useWideViewPort", Boolean.valueOf(enable));
        }
    }

    boolean useWideViewPort() {
        return mUseWideViewPort;
    }

    /**
     * `WebSettings.setLoadWithOverviewMode`: scale the page so its whole width
     * fits the View. It does not change the layout width — that is
     * `useWideViewPort`'s job, exactly as on a phone.
     */
    void setLoadWithOverviewMode(boolean enable) {
        mLoadWithOverviewMode = enable;
        if (mCreated) {
            applySetting("loadWithOverviewMode", Boolean.valueOf(enable));
        }
    }

    boolean loadWithOverviewMode() {
        return mLoadWithOverviewMode;
    }

    String userAgent() {
        return mUserAgent;
    }

    // -- JavaScript ---------------------------------------------------------

    /**
     * `WebView.evaluateJavascript`: Android's contract is **asynchronous** — the
     * call returns and the `ValueCallback` arrives later. This provider used to
     * answer it synchronously over the wire, which meant an app calling it from
     * `onPageFinished` (as almost every app does) blocked the guest's UI thread
     * for as long as that page's script took. WVFAIR measured what that costs: a
     * busy page held the whole guest for ~2 s, the live probe stopped sampling
     * (`NO_FRESH_TREE`) and every other view's input waited with it. The engine
     * round trip now runs on its own thread and the callback is posted back.
     */
    void evaluateJavascript(String script, ValueCallback<String> callback) {
        final JSONObject params = params();
        try {
            params.put("view", mView);
            params.put("script", script);
        } catch (Exception e) {
            if (callback != null) {
                mUi.post(() -> callback.onReceiveValue(null));
            }
            return;
        }
        Thread call = new Thread(() -> {
            String value = null;
            try {
                JSONObject result = mTransport.call("view.evaluateJavascript", params).result();
                value = result == null ? null : result.optString("valueJson", null);
            } catch (Exception e) {
                Log.w(TAG, "evaluateJavascript failed: " + e);
            }
            final String v = value;
            if (callback != null) {
                mUi.post(() -> callback.onReceiveValue(v));
            }
        }, "acos-webview-js");
        call.setDaemon(true);
        call.start();
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
        // A real app calls this right after `new WebView(...)`, before the first
        // measure created the engine view. The interface is guest state until
        // then; `ensureCreated` installs every pending one (WIRE v1.1 §4.2).
        if (!mCreated) {
            return;
        }
        sendJavascriptInterface(name, bridge);
    }

    private void sendJavascriptInterface(String name, JavascriptInterfaceBridge bridge) {
        JSONObject params = params();
        try {
            params.put("view", mView);
            params.put("name", name);
            params.put("methods", new JSONArray(bridge.names()));
            mTransport.call("view.addJavascriptInterface", params);
            Log.i(TAG, "view.addJavascriptInterface " + name + " " + bridge.names());
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
        mFrameDraws.incrementAndGet();
        Bitmap decoded = BitmapFactory.decodeByteArray(png, 0, png.length);
        if (decoded == null) {
            Log.w(TAG, "frame did not decode as an image (" + png.length + " bytes)");
            return;
        }
        mFrame = decoded;
        mFrameSeq = seq;
        mFrameDirty = false;
        if (mFrameDirtyDuringFlight) {
            // The page changed while this frame was being produced: pull again
            // instead of painting the pre-change pixels forever (a tap's down
            // and up invalidations can straddle one render).
            mFrameDirtyDuringFlight = false;
            mFrameDirty = true;
        }
        WebView target = mWebView;
        Log.i(TAG, "frame decoded view=" + mView + " " + decoded.getWidth() + "x"
                + decoded.getHeight() + " seq=" + seq + " bytes=" + png.length
                + " attached=" + (target != null && target.isAttachedToWindow())
                + " handler=" + (target == null ? "null" : String.valueOf(target.getHandler())));
        invalidate();
    }

    /** v1.1: copy the engine's content size / scroll offset into the guest. */
    private void updateEngineMetrics(JSONObject result) {
        if (result == null || !result.has("contentHeight")) {
            return;
        }
        mContentWidth = result.optInt("contentWidth", mContentWidth);
        mContentHeight = result.optInt("contentHeight", mContentHeight);
        mEngineScrollX = result.optInt("scrollX", mEngineScrollX);
        mEngineScrollY = result.optInt("scrollY", mEngineScrollY);
        mHasEngineMetrics = true;
    }

    /**
     * The ax snapshot behind the accessibility provider is stale. The refresh
     * runs on a transport thread: the ACOS UI-tree probe walks View
     * accessibility nodes on the UI thread, and a synchronous `ax` round trip
     * there stalls every frame (measured: the launch live-frame probe timed out
     * with "Skipped 1800 frames"). The provider therefore serves the last known
     * snapshot and refreshes it off the UI thread.
     */
    private void invalidateAxSnapshot() {
        invalidateAxSnapshot(false);
    }

    private void invalidateAxSnapshot(boolean forceSync) {
        synchronized (mAxLock) {
            mAxStale = true;
            if (forceSync) {
                mAxForceSync = true;
            }
        }
        requestAxRefresh();
    }

    private void requestAxRefresh() {
        synchronized (mAxLock) {
            if (mAxInFlight || !mCreated) {
                return;
            }
            mAxInFlight = true;
        }
        final long seq;
        synchronized (mAxLock) {
            seq = ++mAxReadSeq;
        }
        Thread refresh = new Thread(() -> {
            try {
                JSONObject params = params();
                params.put("view", mView);
                JSONObject result = mTransport.call("ax", params).result();
                synchronized (mAxLock) {
                    if (seq >= mAxSnapshotSeq) {
                        mAxSnapshot = result;
                        mAxSnapshotSeq = seq;
                        mAxStale = false;
                    }
                    mAxLock.notifyAll();
                }
            } catch (Exception e) {
                Log.w(TAG, "ax refresh failed: " + e);
            } finally {
                synchronized (mAxLock) {
                    mAxInFlight = false;
                    mAxLock.notifyAll();
                }
            }
        }, "acos-webview-ax");
        refresh.setDaemon(true);
        refresh.start();
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
            Log.i(TAG, "onDraw view=" + mView + " " + w + "x" + h + " dirty=" + mFrameDirty
                    + " frame=" + (mFrame == null ? "none" : "present")
                    + " renderable=" + renderable()
                    + " attached=" + (view != null && view.isAttachedToWindow()));
        }
        if (mBackgroundColor != 0) {
            canvas.drawRect(0, 0, w, h, mBackgroundPaint);
        }
        if (mFrameDraws.get() < 3) {
            android.graphics.Rect clip = new android.graphics.Rect();
            canvas.getClipBounds(clip);
            Log.i(TAG, "onDraw canvas=" + canvas.getClass().getName() + " clip=" + clip
                    + " bg=" + Integer.toHexString(mBackgroundColor));
        }
        Bitmap frame = mFrame;
        if (frame != null) {
            Rect src = new Rect(0, 0, frame.getWidth(), frame.getHeight());
            Rect dst = new Rect(0, 0, w, h);
            try {
                canvas.drawBitmap(frame, src, dst, mPaint);
                if (mFrameDrawLogs.getAndIncrement() < 5) {
                    Log.i(TAG, "drawBitmap " + frame.getWidth() + "x" + frame.getHeight()
                            + " -> " + w + "x" + h + " config=" + frame.getConfig()
                            + " hw=" + canvas.isHardwareAccelerated()
                            + " recycled=" + frame.isRecycled());
                }
            } catch (Throwable t) {
                Log.w(TAG, "drawBitmap failed: " + t, t);
            }
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
        Log.i(TAG, "requestFrame view=" + mView + " " + mWidth + "x" + mHeight + " frame="
                + (mFrame == null ? "none" : mFrame.getWidth() + "x" + mFrame.getHeight())
                + " -> render #" + mFrameRequests);
        Thread pull = new Thread(() -> {
            try {
                AcosWebViewTransport.Frame f = mTransport.render(mView);
                JSONObject result = f.result();
                long seq = result == null ? 0 : result.optLong("seq", 0);
                updateEngineMetrics(result);
                if (f.blob != null && f.blob.length > 0) {
                    setFrame(f.blob, seq);
                } else {
                    Log.w(TAG, "render returned no frame data");
                }
                // A new frame can carry new page text: the previous ax
                // snapshot must not answer the next tree read.
                invalidateAxSnapshot();
            } catch (IOException e) {
                Log.w(TAG, "render failed: " + e);
                // A transient engine state (`FRAME_FAILED`, retryable
                // INTERNAL) must not end the frame pipeline: schedule one more
                // pull after a short delay, bounded so a broken engine fails
                // visibly instead of spinning.
                if (mFrameRetries++ < 20) {
                    mFrameDirty = true;
                    mUi.postDelayed(() -> {
                        invalidate();
                    }, 100);
                }
            } finally {
                synchronized (mFrameLock) {
                    mFrameInFlight = false;
                }
                if (mFrameDirtyDuringFlight) {
                    mUi.postDelayed(() -> {
                        mFrameDirty = true;
                        invalidate();
                    }, 30);
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
            invalidateAxSnapshot(true);
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
     * <p>v1.1 carries the engine's content size and scroll offset in every
     * `render` response (`contentWidth`, `contentHeight`, `scrollX`, `scrollY`),
     * so once one frame has arrived these answer from engine truth. Before the
     * first report the guest's own geometry is the only honest answer: the View
     * is as big as its viewport and has not scrolled. Never a fabricated content
     * height -- that would draw a scrollbar for a range nobody measured.
     */
    int computeHorizontalScrollRange() {
        return mHasEngineMetrics ? Math.max(mContentWidth, mWidth) : mWidth;
    }

    int computeHorizontalScrollOffset() {
        return mHasEngineMetrics ? mEngineScrollX : mScrollX;
    }

    int computeVerticalScrollRange() {
        return mHasEngineMetrics ? Math.max(mContentHeight, mHeight) : mHeight;
    }

    int computeVerticalScrollOffset() {
        return mHasEngineMetrics ? mEngineScrollY : mScrollY;
    }

    int computeVerticalScrollExtent() {
        return mHeight;
    }

    /**
     * `ViewDelegate.onScrollChanged` / `View.scrollTo`. The page's scroll is the
     * engine's; v1.1 `view.scrollTo` moves it and answers with engine metrics,
     * so the guest copies the engine's answer back instead of inventing one.
     */
    void onScrolled(int x, int y) {
        mScrollX = x;
        mScrollY = y;
        if (!mCreated) {
            return;
        }
        JSONObject params = params();
        try {
            params.put("view", mView);
            params.put("x", x);
            params.put("y", y);
            JSONObject result = mTransport.call("view.scrollTo", params).result();
            updateEngineMetrics(result);
        } catch (Exception e) {
            Log.w(TAG, "view.scrollTo failed: " + e);
        }
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
     * `ViewDelegate.onCheckIsTextEditor`: true when the page reports a focused
     * editable field through v1.1 `view.focusChanged` (WIRE.md §5.1). The
     * report is the page's own `document.activeElement`, not a guess.
     */
    boolean isTextEditor() {
        return mTextEditorFocused;
    }

    /**
     * `ViewDelegate.onCreateInputConnection`: an IME connection that commits
     * text through `input.text` (WIRE.md §4.2). Only while the page has a
     * focused editable field; otherwise `null` is the honest answer.
     */
    InputConnection onCreateInputConnection(EditorInfo outAttrs) {
        if (!mTextEditorFocused || mWebView == null) {
            return null;
        }
        if (outAttrs != null) {
            outAttrs.inputType = android.text.InputType.TYPE_CLASS_TEXT
                    | android.text.InputType.TYPE_TEXT_FLAG_MULTI_LINE;
            outAttrs.imeOptions = EditorInfo.IME_ACTION_NONE;
            outAttrs.initialSelStart = 0;
            outAttrs.initialSelEnd = 0;
        }
        return new BaseInputConnection(mWebView, true) {
            @Override
            public boolean commitText(CharSequence text, int newCursorPosition) {
                sendInputText(text == null ? "" : text.toString());
                return true;
            }

            @Override
            public boolean setComposingText(CharSequence text, int newCursorPosition) {
                // v1.1 has no composing-span message; the composed text is sent
                // as committed text rather than pretending a composition exists.
                sendInputText(text == null ? "" : text.toString());
                return true;
            }

            @Override
            public boolean finishComposingText() {
                return true;
            }
        };
    }

    private void sendInputText(String text) {
        if (text.isEmpty()) {
            return;
        }
        JSONObject params = params();
        try {
            params.put("view", mView);
            params.put("text", text);
            mTransport.call("input.text", params);
            invalidateAxSnapshot(true);
        } catch (Exception e) {
            Log.w(TAG, "input.text failed: " + e);
        }
    }

    // -- accessibility: the page's ax tree as virtual View nodes -------------

    /**
     * `ViewDelegate.getAccessibilityNodeProvider` (v1.1): the page's `ax` tree
     * exposed as virtual descendants of the WebView, so the ACOS UI tree (which
     * walks View accessibility node infos) can see page text and tap it.
     */
    AccessibilityNodeProvider getAccessibilityNodeProvider() {
        if (!mCreated) {
            return null;
        }
        synchronized (mAxLock) {
            if (mAxProvider == null) {
                mAxProvider = new AxProvider();
            }
            return mAxProvider;
        }
    }

    private final class AxProvider extends AccessibilityNodeProvider {
        @Override
        public AccessibilityNodeInfo createAccessibilityNodeInfo(int virtualViewId) {
            try {
                JSONObject snapshot = axSnapshot();
                if (snapshot == null) {
                    return null;
                }
                JSONArray nodes = snapshot.optJSONArray("nodes");
                if (nodes == null || nodes.length() == 0) {
                    return virtualViewId == HOST_VIEW_ID ? hostNode() : null;
                }
                int rootIndex = rootIndex(nodes);
                if (virtualViewId == HOST_VIEW_ID) {
                    AccessibilityNodeInfo info = hostNode();
                    for (int i = 0; i < nodes.length(); i++) {
                        if (i == rootIndex) {
                            continue;
                        }
                        JSONObject node = nodes.optJSONObject(i);
                        if (node == null) {
                            continue;
                        }
                        long parent = node.optLong("parent_id", Long.MIN_VALUE);
                        if (parent == Long.MIN_VALUE || parent == nodeId(nodes, rootIndex)
                                || indexOf(nodes, parent) < 0) {
                            info.addChild(mWebView, i + 1);
                        }
                    }
                    return info;
                }
                int index = virtualViewId - 1;
                if (index < 0 || index >= nodes.length() || index == rootIndex) {
                    return null;
                }
                return nodeInfo(nodes, index, rootIndex);
            } catch (Exception e) {
                Log.w(TAG, "ax virtual node " + virtualViewId + " failed: " + e);
                return null;
            }
        }

        @Override
        public java.util.List<AccessibilityNodeInfo> findAccessibilityNodeInfosByText(
                String text, int virtualViewId) {
            java.util.List<AccessibilityNodeInfo> out = new java.util.ArrayList<>();
            try {
                JSONObject snapshot = axSnapshot();
                JSONArray nodes = snapshot == null ? null : snapshot.optJSONArray("nodes");
                if (nodes == null || text == null) {
                    return out;
                }
                String needle = text.toLowerCase(java.util.Locale.ROOT);
                int rootIndex = rootIndex(nodes);
                for (int i = 0; i < nodes.length(); i++) {
                    if (i == rootIndex) {
                        continue;
                    }
                    JSONObject node = nodes.optJSONObject(i);
                    String name = node == null ? "" : optStr(node, "name");
                    if (name.toLowerCase(java.util.Locale.ROOT).contains(needle)) {
                        AccessibilityNodeInfo info = nodeInfo(nodes, i, rootIndex);
                        if (info != null) {
                            out.add(info);
                        }
                    }
                }
            } catch (Exception e) {
                Log.w(TAG, "findAccessibilityNodeInfosByText failed: " + e);
            }
            return out;
        }
    }

    private AccessibilityNodeInfo hostNode() {
        AccessibilityNodeInfo info = AccessibilityNodeInfo.obtain(mWebView);
        info.setClassName("android.webkit.WebView");
        info.setPackageName(mWebView.getContext().getPackageName());
        info.setSource(mWebView);
        int[] origin = new int[2];
        mWebView.getLocationOnScreen(origin);
        Rect rect = new Rect(origin[0], origin[1],
                origin[0] + mWebView.getWidth(), origin[1] + mWebView.getHeight());
        info.setBoundsInScreen(rect);
        info.setVisibleToUser(true);
        info.setEnabled(mWebView.isEnabled());
        info.setFocusable(true);
        info.setFocused(mViewFocused);
        info.setScrollable(false);
        return info;
    }

    private AccessibilityNodeInfo nodeInfo(JSONArray nodes, int index, int rootIndex) {
        JSONObject node = nodes.optJSONObject(index);
        if (node == null) {
            return null;
        }
        AccessibilityNodeInfo info = AccessibilityNodeInfo.obtain(mWebView, index + 1);
        String role = node.optString("role", "");
        info.setClassName(androidClass(role));
        info.setPackageName(mWebView.getContext().getPackageName());
        // `org.json.optString` turns a JSON null into the string "null"; the
        // ax export uses null for "no name/value", so read it explicitly.
        String name = optStr(node, "name");
        String value = optStr(node, "value");
        String description = optStr(node, "description");
        String text = value.isEmpty() ? name : value;
        info.setText(text);
        info.setContentDescription(description);
        info.setClickable(node.optBoolean("clickable", false));
        info.setEnabled(!node.optBoolean("disabled", false));
        info.setFocusable(true);
        info.setFocused(node.optBoolean("focused", false));
        info.setEditable(node.optBoolean("editable", false));
        boolean scrollable = "list".equals(role) || "listbox".equals(role)
                || "document".equals(role) || "RootWebArea".equals(role);
        info.setScrollable(scrollable);
        String checked = node.has("checked") && !node.isNull("checked")
                ? node.optString("checked", "") : null;
        if (checked != null && !checked.isEmpty()) {
            info.setCheckable(true);
            info.setChecked("true".equals(checked) || "mixed".equals(checked));
        }
        boolean hasBounds = node.optBoolean("has_bounds", false);
        int[] origin = new int[2];
        mWebView.getLocationOnScreen(origin);
        Rect rect = new Rect(0, 0, 0, 0);
        if (hasBounds) {
            JSONArray b = node.optJSONArray("bounds");
            if (b != null && b.length() >= 4) {
                int x = (int) Math.round(b.optDouble(0, 0));
                int y = (int) Math.round(b.optDouble(1, 0));
                int w = (int) Math.round(b.optDouble(2, 0));
                int h = (int) Math.round(b.optDouble(3, 0));
                rect.set(origin[0] + x, origin[1] + y, origin[0] + x + w, origin[1] + y + h);
            }
        }
        info.setBoundsInScreen(rect);
        info.setVisibleToUser(hasBounds && rect.width() > 0 && rect.height() > 0);
        // Parent and children, in the same virtual id space.
        long parent = node.optLong("parent_id", Long.MIN_VALUE);
        int parentIndex = parent == Long.MIN_VALUE ? -1 : indexOf(nodes, parent);
        if (parentIndex < 0 || parentIndex == rootIndex) {
            info.setParent(mWebView);
        } else {
            info.setParent(mWebView, parentIndex + 1);
        }
        long id = nodeId(nodes, index);
        for (int i = 0; i < nodes.length(); i++) {
            if (i == rootIndex) {
                continue;
            }
            JSONObject child = nodes.optJSONObject(i);
            if (child != null && child.optLong("parent_id", Long.MIN_VALUE) == id) {
                info.addChild(mWebView, i + 1);
            }
        }
        return info;
    }

    /**
     * WVFAIR: how long a *post-input* tree read may wait for the engine.
     *
     * The read runs on the UI thread (`AccessibilityNodeProvider`), and the
     * screen tree is every view's tree, so an unbounded wait for one page's
     * accessibility tree stalls the whole guest: the probe's tree sample went
     * missing for seconds while the second WebView was busy (`NO_FRESH_TREE`,
     * measured). A page that cannot answer its tree in this window is served
     * from the last snapshot, and the refresh keeps running in the background.
     */
    private static final long AX_SYNC_WAIT_MS = 250;

    private JSONObject axSnapshot() {
        boolean force;
        synchronized (mAxLock) {
            force = mAxForceSync;
            mAxForceSync = false;
            if (!force && mAxSnapshot != null && !mAxStale) {
                return mAxSnapshot;
            }
        }
        if (force) {
            // A tree read right after an input: answer from the engine so the
            // step cannot observe the pre-input text — but **bounded**, so a
            // page whose main thread is busy cannot hold the guest's UI thread
            // (and with it every other view's tree and every input) hostage.
            final JSONObject[] answer = new JSONObject[1];
            final java.util.concurrent.CountDownLatch done =
                    new java.util.concurrent.CountDownLatch(1);
            final long seq;
            synchronized (mAxLock) {
                seq = ++mAxReadSeq;
            }
            Thread read = new Thread(() -> {
                try {
                    JSONObject params = params();
                    params.put("view", mView);
                    JSONObject result = mTransport.call("ax", params).result();
                    synchronized (mAxLock) {
                        // A later read's answer wins: an abandoned read that
                        // finishes late must not put back an older tree.
                        if (seq >= mAxSnapshotSeq) {
                            mAxSnapshot = result;
                            mAxSnapshotSeq = seq;
                            mAxStale = false;
                        }
                    }
                    answer[0] = result;
                } catch (Exception e) {
                    Log.w(TAG, "sync ax failed: " + e);
                } finally {
                    done.countDown();
                }
            }, "acos-webview-ax-sync");
            read.setDaemon(true);
            read.start();
            try {
                done.await(AX_SYNC_WAIT_MS, java.util.concurrent.TimeUnit.MILLISECONDS);
            } catch (InterruptedException e) {
                Thread.currentThread().interrupt();
            }
            if (answer[0] != null) {
                return answer[0];
            }
            Log.w(TAG, "ax: post-input refresh did not answer in " + AX_SYNC_WAIT_MS
                    + " ms (view=" + mView + "); serving the last snapshot");
        }
        requestAxRefresh();
        synchronized (mAxLock) {
            return mAxSnapshot;
        }
    }

    private static String optStr(JSONObject node, String key) {
        if (node == null || !node.has(key) || node.isNull(key)) {
            return "";
        }
        return node.optString(key, "");
    }

    private static int rootIndex(JSONArray nodes) {
        for (int i = 0; i < nodes.length(); i++) {
            JSONObject node = nodes.optJSONObject(i);
            if (node != null && "RootWebArea".equals(node.optString("role", ""))) {
                return i;
            }
        }
        for (int i = 0; i < nodes.length(); i++) {
            JSONObject node = nodes.optJSONObject(i);
            if (node != null && !node.has("parent_id")) {
                return i;
            }
        }
        return -1;
    }

    private static long nodeId(JSONArray nodes, int index) {
        JSONObject node = index < 0 ? null : nodes.optJSONObject(index);
        return node == null ? Long.MIN_VALUE : node.optLong("node_id", Long.MIN_VALUE);
    }

    private static int indexOf(JSONArray nodes, long nodeId) {
        for (int i = 0; i < nodes.length(); i++) {
            JSONObject node = nodes.optJSONObject(i);
            if (node != null && node.optLong("node_id", Long.MIN_VALUE) == nodeId) {
                return i;
            }
        }
        return -1;
    }

    private static String androidClass(String role) {
        switch (role) {
            case "button":
                return "android.widget.Button";
            case "textbox":
            case "textField":
            case "searchbox":
                return "android.widget.EditText";
            case "StaticText":
            case "heading":
            case "link":
                return "android.widget.TextView";
            case "image":
                return "android.widget.ImageView";
            case "list":
                return "android.widget.ListView";
            default:
                return "android.view.View";
        }
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
                    if (mFrameInFlight) {
                        mFrameDirtyDuringFlight = true;
                    }
                    mFrameDirty = true;
                    invalidateAxSnapshot();
                    invalidate();
                    break;
                }
                case "view.focusChanged": {
                    // v1.1: the page's focused editable field, reported by the
                    // engine. It answers `onCheckIsTextEditor` and gates the IME
                    // connection; the ax snapshot is invalidated with it.
                    mTextEditorFocused = event.optBoolean("focused", false)
                            && event.optBoolean("editable", false);
                    mFocusedFieldType = event.optString("type", "");
                    invalidateAxSnapshot();
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
            if ("req.intercept".equals(op)) {
                return intercept(params);
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

        /**
         * v1.1 / WIRE.md §4.3: serve `file:///android_asset/*` from this APK's
         * assets, so a page loads without the bytes ever leaving the guest
         * except through this channel. `content://` has no guest resolver here
         * and fails loudly; http(s) continue to Chromium.
         */
        private JSONObject intercept(JSONObject params) {
            String url = params.optString("url", "");
            JSONObject result = new JSONObject();
            try {
                String prefix = "file:///android_asset/";
                if (url.startsWith(prefix)) {
                    String path = url.substring(prefix.length());
                    int cut = path.indexOf('?');
                    if (cut >= 0) {
                        path = path.substring(0, cut);
                    }
                    int hash = path.indexOf('#');
                    if (hash >= 0) {
                        path = path.substring(0, hash);
                    }
                    path = android.net.Uri.decode(path);
                    byte[] body;
                    try {
                        InputStream in = mWebView.getContext().getAssets().open(path);
                        try {
                            ByteArrayOutputStream out = new ByteArrayOutputStream();
                            byte[] chunk = new byte[8192];
                            int n;
                            while ((n = in.read(chunk)) > 0) {
                                out.write(chunk, 0, n);
                            }
                            body = out.toByteArray();
                        } finally {
                            in.close();
                        }
                    } catch (IOException missing) {
                        result.put("action", "fail");
                        result.put("reason", "net::ERR_FILE_NOT_FOUND");
                        return result;
                    }
                    result.put("action", "fulfill");
                    result.put("status", 200);
                    result.put("mime", mimeOf(path));
                    JSONObject headers = new JSONObject();
                    headers.put("access-control-allow-origin", "*");
                    result.put("headers", headers);
                    result.put("bodyB64", Base64.encodeToString(body, Base64.NO_WRAP));
                    Log.i(TAG, "req.intercept asset " + path + " " + body.length + " bytes");
                    return result;
                }
                if (url.startsWith("content://")) {
                    result.put("action", "fail");
                    result.put("reason", "net::ERR_UNKNOWN_URL_SCHEME");
                    return result;
                }
                result.put("action", "continue");
                return result;
            } catch (Exception e) {
                try {
                    result.put("action", "fail");
                    result.put("reason", "net::ERR_FAILED");
                } catch (org.json.JSONException ignored) {
                }
                Log.w(TAG, "req.intercept failed: " + e);
                return result;
            }
        }

        private String mimeOf(String path) {
            String lower = path.toLowerCase(java.util.Locale.ROOT);
            if (lower.endsWith(".html") || lower.endsWith(".htm")) {
                return "text/html";
            }
            if (lower.endsWith(".js")) {
                return "application/javascript";
            }
            if (lower.endsWith(".css")) {
                return "text/css";
            }
            if (lower.endsWith(".json")) {
                return "application/json";
            }
            if (lower.endsWith(".svg")) {
                return "image/svg+xml";
            }
            if (lower.endsWith(".png")) {
                return "image/png";
            }
            return "application/octet-stream";
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
