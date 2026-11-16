package dev.acos.fixtures.a11yprobe;

import android.accessibilityservice.AccessibilityService;
import android.accessibilityservice.AccessibilityServiceInfo;
import android.accessibilityservice.GestureDescription;
import android.graphics.Bitmap;
import android.graphics.ColorSpace;
import android.graphics.Path;
import android.graphics.PixelFormat;
import android.graphics.Rect;
import android.hardware.HardwareBuffer;
import android.os.Handler;
import android.os.Looper;
import android.os.Process;
import android.os.SystemClock;
import android.util.Log;
import android.view.Gravity;
import android.view.WindowManager;
import android.widget.TextView;
import android.view.View;
import android.view.accessibility.AccessibilityEvent;
import android.view.accessibility.AccessibilityNodeInfo;
import android.view.accessibility.AccessibilityWindowInfo;

import java.util.ArrayList;
import java.util.concurrent.Executor;

import java.util.List;

/**
 * The probe's declared accessibility service.
 *
 * <p>MODA11Y2: the module binds this service the way
 * {@code AccessibilityManagerService} does, so {@link #onServiceConnected}
 * runs with the {@link AccessibilityServiceInfo} the system handed over, and
 * the connection answers {@code getServiceInfo()} — both are logged so the
 * scenario can assert the bind instead of trusting the module's report.
 *
 * <p>MODA11Y4: the service then exercises the real service-facing interaction
 * surface — {@code getWindows()}, {@code getRootInActiveWindow()},
 * a by-view-id node query and {@code ACTION_CLICK} — and every answer is
 * logged. The click lands in {@code MainActivity}'s own listener, so the app's
 * screen (the {@code status} text) proves the action ran in the app, not in the
 * probe's bookkeeping.
 *
 * <p>The interrogation runs on a worker thread, not on the service's main
 * thread. The guest is a single process: the app-side
 * {@code AccessibilityInteractionController} handles a request on the app's
 * main-thread handler, and AOSP's same-thread fast path
 * ({@code AccessibilityInteractionController.scheduleMessage}: interrogating
 * pid == my pid && interrogating tid == my looper tid → the waiting client
 * handles the message inline) needs the interrogating pid, which ACOS's binder
 * does not model (no caller pid/uid anywhere). Blocking the main thread would
 * therefore wait out the framework's 5 s interaction timeout. Querying from a
 * worker thread is what a real service does anyway (never block the main
 * thread on a synchronous a11y query).
 */
public class A11yProbeService extends AccessibilityService {
    private static final String TAG = "AcosA11yProbe";

    /** The live service, so {@code MainActivity} can ask it for a global action. */
    private static volatile A11yProbeService sInstance;

    /** The view id the probe clicks (`MainActivity`'s {@code click_target}). */
    private static final String TARGET_VIEW_ID =
            "dev.acos.fixtures.a11yprobe:id/click_target";

    @Override
    protected void onServiceConnected() {
        AccessibilityServiceInfo info = getServiceInfo();
        String component = info == null || info.getId() == null ? "?" : info.getId();
        // MODA11Y6: capabilities and event types come from the
        // `<accessibility-service>` meta-data block the module parsed out of
        // the APK, which is also what gates the interaction surface.
        // MODA11Y6: capabilities come from the `<accessibility-service>` block
        // the module parsed out of the APK, which is also what gates the
        // interaction surface.
        Log.i(TAG, "service_connected id=" + component
                + " capabilities=" + (info == null ? -999 : info.getCapabilities()));
        sInstance = this;
        Thread probe = new Thread(this::probe, "a11y-probe");
        probe.setDaemon(true);
        probe.start();
    }

    /**
     * `performGlobalAction(GLOBAL_ACTION_BACK)` on a worker thread (never on the
     * caller's thread: the app is deciding whether to finish).
     */
    static void requestBack() {
        final A11yProbeService service = sInstance;
        if (service == null) {
            Log.i(TAG, "global_action back=false reason=not_connected");
            return;
        }
        Thread back = new Thread(() -> {
            boolean ok = service.performGlobalAction(GLOBAL_ACTION_BACK);
            Log.i(TAG, "global_action back=" + ok);
        }, "a11y-back");
        back.setDaemon(true);
        back.start();
    }

    /** Query windows, the active root, one node, and click it. */
    private void probe() {
        Log.i(TAG, "probe_start pid=" + Process.myPid()
                + " tid=" + Thread.currentThread().getId());

        List<AccessibilityWindowInfo> windows = null;
        int attempts = 0;
        for (; attempts < 25; attempts++) {
            windows = getWindows();
            if (windows != null && !windows.isEmpty()) {
                break;
            }
            SystemClock.sleep(200);
        }
        attempts++;
        int count = windows == null ? -1 : windows.size();
        Log.i(TAG, "windows count=" + count + " attempts=" + attempts);
        if (windows != null) {
            for (int i = 0; i < windows.size(); i++) {
                AccessibilityWindowInfo window = windows.get(i);
                Log.i(TAG, "window[" + i + "] id=" + window.getId()
                        + " type=" + window.getType()
                        + " layer=" + window.getLayer()
                        + " active=" + window.isActive()
                        + " focused=" + window.isFocused()
                        + " title=" + window.getTitle());
            }
        }

        AccessibilityNodeInfo root = getRootInActiveWindow();
        Log.i(TAG, "root " + describe(root));
        // MODA11YNATIVE: the gesture and screenshot probes run on this same
        // root, taken BEFORE the overlay is added. A query after the add would
        // go through the service's own overlay window connection on this worker
        // thread, which the framework answers on the main thread — the same
        // same-process interaction wait MODA11Y4 documented.
        Rect gestureTarget = boundsOf(root, "dev.acos.fixtures.a11yprobe:id/gesture_target");
        Rect gestureList = boundsOf(root, "dev.acos.fixtures.a11yprobe:id/gesture_list");
        Rect swatch = boundsOf(root, "dev.acos.fixtures.a11yprobe:id/swatch");

        if (root != null) {
            List<AccessibilityNodeInfo> byId =
                    root.findAccessibilityNodeInfosByViewId(TARGET_VIEW_ID);
            Log.i(TAG, "by_view_id count=" + (byId == null ? -1 : byId.size()));
            if (byId != null && !byId.isEmpty()) {
                AccessibilityNodeInfo target = byId.get(0);
                Log.i(TAG, "node " + describe(target));
                boolean clicked = target.performAction(AccessibilityNodeInfo.ACTION_CLICK);
                Log.i(TAG, "click handled=" + clicked);
            }
        }
        Log.i(TAG, "probe_done");

        probeGestures(gestureTarget, gestureList);

        // MODA11Y5 step 4: the same service adds a TYPE_ACCESSIBILITY_OVERLAY
        // window through the WindowManager the framework gave it. AOSP fills
        // the LayoutParams token from the token `IAccessibilityServiceClient
        // .init(connectionId, windowToken)` handed the service
        // (AccessibilityService.java:2660-2670, 2803-2812), and the WMS accepts
        // an accessibility overlay only on that token
        // (WindowManagerService.java:1630-1640, sanitizeWindowType 9914-9928).
        addOverlay();
        // MODA11Y5: a second `getWindows()` from this session never returned
        // (the window-session thread held the window-manager state while the
        // overlay's own `relayout`/`finishDrawing` was in flight, and this
        // handler wants the same state). MODA11Y6: the guest re-query is back —
        // it is the assertion that the overlay is a real window of type
        // TYPE_ACCESSIBILITY_OVERLAY as the *service* sees it, not only in the
        // host's stack.
        Log.i(TAG, "windows2_start");
        List<AccessibilityWindowInfo> again = null;
        boolean overlay = false;
        int attempts2 = 0;
        for (; attempts2 < 25; attempts2++) {
            again = getWindows();
            overlay = false;
            if (again != null) {
                for (int i = 0; i < again.size(); i++) {
                    overlay |= again.get(i).getType()
                            == AccessibilityWindowInfo.TYPE_ACCESSIBILITY_OVERLAY;
                }
            }
            if (overlay) {
                break;
            }
            SystemClock.sleep(200);
        }
        int count2 = again == null ? -1 : again.size();
        Log.i(TAG, "windows2 count=" + count2 + " attempts=" + (attempts2 + 1));
        if (again != null) {
            for (int i = 0; i < again.size(); i++) {
                AccessibilityWindowInfo window = again.get(i);
                Log.i(TAG, "window2[" + i + "] id=" + window.getId()
                        + " type=" + window.getType()
                        + " layer=" + window.getLayer()
                        + " active=" + window.isActive()
                        + " focused=" + window.isFocused());
            }
        }
        Log.i(TAG, "windows2 overlay=" + overlay);
        Log.i(TAG, "overlay_probe_done");

        probeScreenshots(swatch == null ? 400 : swatch.centerY());
    }

    /**
     * MODA11YNATIVE step 1: `dispatchGesture`.
     *
     * <p>The framework's own `AccessibilityService.dispatchGesture` samples the
     * `GestureDescription` into `MotionEvent`s
     * (`GestureDescription$MotionEventGenerator`) and puts them in a
     * `ParceledListSlice`, so what leaves this process is the real wire. The
     * tap targets the {@code gesture_target} button and the swipe drags the
     * {@code gesture_list} list; the app's own listeners count both, so the
     * scenario's assertion is the app's state, not this log.
     */
    private void probeGestures(Rect tapBounds, Rect listBounds) {
        if (tapBounds == null) {
            Log.i(TAG, "gesture tap=false reason=no_gesture_target");
        } else {
            Path tap = new Path();
            tap.moveTo(tapBounds.exactCenterX(), tapBounds.exactCenterY());
            GestureDescription description = new GestureDescription.Builder()
                    .addStroke(new GestureDescription.StrokeDescription(tap, 0, 40))
                    .build();
            dispatch("tap", description);
        }
        if (listBounds == null) {
            Log.i(TAG, "gesture swipe=false reason=no_gesture_list");
        } else {
            float x = listBounds.exactCenterX();
            Path swipe = new Path();
            swipe.moveTo(x, listBounds.bottom - 10);
            swipe.lineTo(x, listBounds.top + 10);
            GestureDescription description = new GestureDescription.Builder()
                    .addStroke(new GestureDescription.StrokeDescription(swipe, 0, 200))
                    .build();
            dispatch("swipe", description);
        }
    }

    /** Dispatch one description and log the framework's own result callback. */
    private void dispatch(String kind, GestureDescription description) {
        boolean accepted = dispatchGesture(description,
                new GestureResultCallback() {
                    @Override
                    public void onCompleted(GestureDescription gesture) {
                        Log.i(TAG, "gesture_result kind=" + kind + " completed=true");
                    }

                    @Override
                    public void onCancelled(GestureDescription gesture) {
                        Log.i(TAG, "gesture_result kind=" + kind + " completed=false");
                    }
                }, new Handler(Looper.getMainLooper()));
        Log.i(TAG, "gesture_sent kind=" + kind + " accepted=" + accepted
                + " strokes=" + description.getStrokeCount());
    }

    private static Rect boundsOf(AccessibilityNodeInfo root, String viewId) {
        if (root == null) {
            return null;
        }
        List<AccessibilityNodeInfo> found = root.findAccessibilityNodeInfosByViewId(viewId);
        if (found == null || found.isEmpty()) {
            return null;
        }
        Rect bounds = new Rect();
        found.get(0).getBoundsInScreen(bounds);
        return bounds;
    }

    /**
     * MODA11YNATIVE step 2: `takeScreenshot` and its failure paths.
     *
     * <p>Everything asserted here is the framework's own callback: AOSP's
     * `AbstractAccessibilityServiceConnection.takeScreenshot` answers the 333 ms
     * interval with `ERROR_TAKE_SCREENSHOT_INTERVAL_TIME_SHORT` (3), an unknown
     * display with `…INVALID_DISPLAY` (4) and a screen showing a
     * `FLAG_SECURE` window with `…SECURE_WINDOW` (6), all through
     * `TakeScreenshotCallback.onFailure`. The last call succeeds and reads one
     * pixel of the `#FF3366CC` swatch out of the returned `HardwareBuffer`.
     */
    private void probeScreenshots(int swatchCenterY) {
        this.swatchRow = swatchCenterY;
        final Executor executor = getMainExecutor();
        // 1. The success path first: it also arms the 333 ms interval.
        takeScreenshot(0, executor, screenshotCallback("capture", true));
        // 2. Immediately again: inside the interval, so AOSP answers 3.
        takeScreenshot(0, executor, screenshotCallback("interval", false));
        SystemClock.sleep(400);
        // 3. A display this device does not have: 4.
        takeScreenshot(5, executor, screenshotCallback("display", false));
        SystemClock.sleep(400);
        // 4. A FLAG_SECURE window on screen: 6.
        addSecureOverlay(true);
        SystemClock.sleep(400);
        takeScreenshot(0, executor, screenshotCallback("secure", false));
        SystemClock.sleep(400);
        addSecureOverlay(false);
    }

    private TakeScreenshotCallback screenshotCallback(String kind, boolean success) {
        return new TakeScreenshotCallback() {
            @Override
            public void onSuccess(ScreenshotResult result) {
                HardwareBuffer buffer = result.getHardwareBuffer();
                ColorSpace colorSpace = result.getColorSpace();
                Log.i(TAG, "screenshot_result kind=" + kind + " status=0"
                        + " width=" + buffer.getWidth()
                        + " height=" + buffer.getHeight()
                        + " format=" + buffer.getFormat()
                        + " usage=" + buffer.getUsage()
                        + " color_space=" + (colorSpace == null ? "null" : colorSpace.getName())
                        + " timestamp=" + result.getTimestamp());
                Log.i(TAG, "screenshot_expect kind=" + kind + " success=" + success);
                readPixel(buffer, colorSpace);
                buffer.close();
            }

            @Override
            public void onFailure(int errorCode) {
                Log.i(TAG, "screenshot_result kind=" + kind + " status=" + errorCode);
                Log.i(TAG, "screenshot_expect kind=" + kind + " success=" + success);
            }
        };
    }

    /**
     * Wrap the `HardwareBuffer` the way an app does and read one pixel of the
     * swatch (200x80 dp at the top centre of a 1080x2400 screen; the frame is
     * display-sized, so the pixel is read at the display centre X and at the
     * swatch's own row).
     */
    private void readPixel(HardwareBuffer buffer, ColorSpace colorSpace) {
        try {
            Bitmap bitmap = Bitmap.wrapHardwareBuffer(buffer, colorSpace);
            if (bitmap == null) {
                Log.i(TAG, "screenshot_bitmap null");
                return;
            }
            Log.i(TAG, "screenshot_bitmap width=" + bitmap.getWidth()
                    + " height=" + bitmap.getHeight()
                    + " config=" + bitmap.getConfig());
            // A hardware bitmap has no CPU pixels; the copy is the app-side way
            // to read one (`Bitmap.copy`).
            Bitmap software = bitmap.copy(Bitmap.Config.ARGB_8888, false);
            if (software == null) {
                Log.i(TAG, "screenshot_pixel null reason=no_software_copy");
                return;
            }
            int x = software.getWidth() / 2;
            int y = swatchRow;
            int pixel = software.getPixel(x, y);
            Log.i(TAG, "screenshot_pixel x=" + x + " y=" + y
                    + " color=" + String.format("#%08X", pixel));
        } catch (Throwable failure) {
            Log.i(TAG, "screenshot_pixel error=" + failure);
        }
    }

    /** The swatch's centre row, read from the app's tree before the overlay. */
    private int swatchRow = 400;

    /**
     * Add (or remove) a {@code FLAG_SECURE} accessibility overlay.
     *
     * <p>This is what makes the secure-window failure reachable on a device with
     * no DRM app: the overlay is a real window of the service's own with
     * `WindowManager.LayoutParams.FLAG_SECURE`, so the compositor's
     * `has_secure_window()` sees it and `takeScreenshot` answers
     * `ERROR_TAKE_SCREENSHOT_SECURE_WINDOW`.
     */
    private boolean secureOverlayAdded;
    private View secureOverlayView;
    private WindowManager secureOverlayWindowManager;
    private void addSecureOverlay(boolean add) {
        final A11yProbeService service = this;
        final java.util.concurrent.CountDownLatch done =
                new java.util.concurrent.CountDownLatch(1);
        new Handler(Looper.getMainLooper()).post(() -> {
            try {
                WindowManager wm = (WindowManager) service.getSystemService(WINDOW_SERVICE);
                secureOverlayWindowManager = wm;
                if (add) {
                    TextView view = new TextView(service);
                    view.setText("secure");
                    WindowManager.LayoutParams params = new WindowManager.LayoutParams(
                            120, 60,
                            WindowManager.LayoutParams.TYPE_ACCESSIBILITY_OVERLAY,
                            WindowManager.LayoutParams.FLAG_NOT_FOCUSABLE
                                    | WindowManager.LayoutParams.FLAG_NOT_TOUCHABLE
                                    | WindowManager.LayoutParams.FLAG_SECURE,
                            PixelFormat.TRANSLUCENT);
                    params.gravity = Gravity.TOP | Gravity.START;
                    params.x = 400;
                    params.y = 600;
                    wm.addView(view, params);
                    secureOverlayView = view;
                    secureOverlayAdded = true;
                    Log.i(TAG, "secure_overlay added=true flags=" + params.flags);
                } else if (secureOverlayAdded) {
                    // Removed again, so the window stack the scenario asserts on
                    // is back to the application window plus the one
                    // non-secure overlay the MODA11Y5 step added.
                    wm.removeView(secureOverlayView);
                    secureOverlayView = null;
                    secureOverlayAdded = false;
                    Log.i(TAG, "secure_overlay added=false removed=true");
                }
            } catch (Throwable failure) {
                Log.i(TAG, "secure_overlay added=" + add + " error=" + failure);
            } finally {
                done.countDown();
            }
        });
        try {
            done.await(5, java.util.concurrent.TimeUnit.SECONDS);
        } catch (InterruptedException interrupted) {
            Thread.currentThread().interrupt();
        }
    }

    /**
     * Add one small {@code TYPE_ACCESSIBILITY_OVERLAY} view on the service's
     * overlay window token.
     *
     * <p>The add runs on the main thread: {@code WindowManagerImpl.addView}
     * creates a {@code ViewRootImpl} in the calling thread, which needs a
     * {@code Looper}, and the probe itself runs on a worker thread.
     */
    private void addOverlay() {
        final A11yProbeService service = this;
        final java.util.concurrent.CountDownLatch added =
                new java.util.concurrent.CountDownLatch(1);
        new Handler(Looper.getMainLooper()).post(() -> {
            try {
                WindowManager wm = (WindowManager) service.getSystemService(WINDOW_SERVICE);
                TextView view = new TextView(service);
                view.setText("a11y overlay");
                view.setBackgroundColor(0x80FF0000);
                WindowManager.LayoutParams params = new WindowManager.LayoutParams(
                        240, 120,
                        WindowManager.LayoutParams.TYPE_ACCESSIBILITY_OVERLAY,
                        WindowManager.LayoutParams.FLAG_NOT_FOCUSABLE
                                | WindowManager.LayoutParams.FLAG_NOT_TOUCHABLE,
                        PixelFormat.TRANSLUCENT);
                params.gravity = Gravity.TOP | Gravity.START;
                params.x = 40;
                params.y = 240;
                wm.addView(view, params);
                Log.i(TAG, "overlay_added type=" + params.type
                        + " token=" + (params.token == null ? "null" : "set"));
            } catch (Throwable failure) {
                Log.i(TAG, "overlay_added false reason=" + failure);
            } finally {
                added.countDown();
            }
        });
        try {
            added.await(5, java.util.concurrent.TimeUnit.SECONDS);
        } catch (InterruptedException interrupted) {
            Thread.currentThread().interrupt();
        }
    }

    /** `class=… text=… view_id=… clickable=…` for one node. */
    private static String describe(AccessibilityNodeInfo node) {
        if (node == null) {
            return "null";
        }
        return "class=" + node.getClassName()
                + " text=" + node.getText()
                + " view_id=" + node.getViewIdResourceName()
                + " clickable=" + node.isClickable();
    }

    @Override
    public void onAccessibilityEvent(AccessibilityEvent event) {
        Log.i(TAG, "onAccessibilityEvent type=" + event.getEventType());
    }

    @Override
    public void onInterrupt() {
        Log.i(TAG, "onInterrupt");
    }
}
