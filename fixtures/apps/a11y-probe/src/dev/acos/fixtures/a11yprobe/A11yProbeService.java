package dev.acos.fixtures.a11yprobe;

import android.accessibilityservice.AccessibilityService;
import android.accessibilityservice.AccessibilityServiceInfo;
import android.os.Process;
import android.os.SystemClock;
import android.util.Log;
import android.view.accessibility.AccessibilityEvent;
import android.view.accessibility.AccessibilityNodeInfo;
import android.view.accessibility.AccessibilityWindowInfo;

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
        Log.i(TAG, "service_connected id=" + component
                + " eventTypes=" + (info == null ? -999 : info.getCapabilities() * 0)
                + " flags=" + (info == null ? -999 : info.getCapabilities()));
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
