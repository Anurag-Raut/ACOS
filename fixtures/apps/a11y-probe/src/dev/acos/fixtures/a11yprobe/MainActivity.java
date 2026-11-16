package dev.acos.fixtures.a11yprobe;

import android.accessibilityservice.AccessibilityServiceInfo;
import android.app.Activity;
import android.content.Context;
import android.os.Bundle;
import android.util.Log;
import android.view.accessibility.AccessibilityEvent;
import android.view.accessibility.AccessibilityManager;
import android.widget.Button;
import android.widget.TextView;

import java.util.List;

/**
 * ADR-0029 accessibility probe: reads the real {@link AccessibilityManager}
 * state and sends one real event.
 *
 * <p>The module's launch config names {@code dev.acos.fixtures.a11yprobe/.A11yProbeService};
 * with it attached, {@code isEnabled()} is true, the installed/enabled service
 * lists contain that component, and the event reaches
 * {@code IAccessibilityManager.sendAccessibilityEvent} (the module records it).
 */
public class MainActivity extends Activity {
    private static final String TAG = "AcosA11yProbe";
    private AccessibilityManager manager;
    private TextView status;
    private int sent;

    @Override
    protected void onCreate(Bundle savedInstanceState) {
        super.onCreate(savedInstanceState);
        setContentView(R.layout.main);
        manager = (AccessibilityManager) getSystemService(Context.ACCESSIBILITY_SERVICE);
        status = (TextView) findViewById(R.id.status);
        updateStatus();

        Button send = (Button) findViewById(R.id.send);
        send.setOnClickListener(v -> {
            AccessibilityEvent event = AccessibilityEvent.obtain(
                    AccessibilityEvent.TYPE_VIEW_CLICKED);
            event.setPackageName(getPackageName());
            event.setClassName("android.widget.Button");
            manager.sendAccessibilityEvent(event);
            sent++;
            Log.i(TAG, "event_sent type=" + event.getEventType() + " count=" + sent);
            updateStatus();
        });
    }

    private void updateStatus() {
        boolean enabled = manager.isEnabled();
        List<AccessibilityServiceInfo> installed =
                manager.getInstalledAccessibilityServiceList();
        List<AccessibilityServiceInfo> enabledList =
                manager.getEnabledAccessibilityServiceList(
                        AccessibilityServiceInfo.FEEDBACK_ALL_MASK);
        int enabledCount = enabledList == null ? -1 : enabledList.size();
        String first = "";
        if (installed != null && !installed.isEmpty()
                && installed.get(0).getResolveInfo() == null
                && installed.get(0).getId() != null) {
            first = " first=" + installed.get(0).getId();
        }
        String text = "a11y enabled=" + enabled
                + " installed=" + (installed == null ? -1 : installed.size())
                + " enabled_list=" + enabledCount
                + " sent=" + sent + first;
        status.setText(text);
        Log.i(TAG, "status " + text);
    }
}
