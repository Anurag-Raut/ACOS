package dev.acos.fixtures.a11yprobe;

import android.accessibilityservice.AccessibilityService;
import android.util.Log;
import android.view.accessibility.AccessibilityEvent;

/** The probe's declared accessibility service (not bound yet; ADR-0029). */
public class A11yProbeService extends AccessibilityService {
    private static final String TAG = "AcosA11yProbe";

    @Override
    public void onAccessibilityEvent(AccessibilityEvent event) {
        Log.i(TAG, "onAccessibilityEvent type=" + event.getEventType());
    }

    @Override
    public void onInterrupt() {
        Log.i(TAG, "onInterrupt");
    }
}
