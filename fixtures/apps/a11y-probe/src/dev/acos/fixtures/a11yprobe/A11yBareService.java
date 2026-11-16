package dev.acos.fixtures.a11yprobe;

import android.accessibilityservice.AccessibilityService;
import android.util.Log;
import android.view.accessibility.AccessibilityEvent;

/**
 * MODA11Y6 step 2, the negative case: an accessibility service that declares
 * the meta-data block **without** `android:canRetrieveWindowContent` and
 * without `flagRetrieveInteractiveWindows`.
 *
 * <p>It exists so both the host and the ART probe can ask the same questions of
 * a service that has no capability: AOSP's `AccessibilitySecurityPolicy` gives
 * it `null` for `getWindows`/`getWindow`/node queries and `false` for actions
 * (`AccessibilityServiceConnection.getWindows`,
 * `AbstractAccessibilityServiceConnection.findAccessibilityNodeInfo*`), never
 * the window list and never an exception.
 */
public class A11yBareService extends AccessibilityService {
    private static final String TAG = "AcosA11yBare";

    @Override
    protected void onServiceConnected() {
        Log.i(TAG, "bare_service_connected");
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
