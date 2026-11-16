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
 *
 * <p>MODA11Y4: {@code click_target} is the node the probe's accessibility
 * service clicks through {@code ACTION_CLICK}. The counter lives in this
 * activity and is rendered into the {@code status} text, so the frame shows the
 * click happened in the app.
 */
public class MainActivity extends Activity {
    private static final String TAG = "AcosA11yProbe";
    private AccessibilityManager manager;
    private TextView status;
    private int sent;
    private int clicks;

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

        Button target = (Button) findViewById(R.id.click_target);
        target.setOnClickListener(v -> {
            clicks++;
            Log.i(TAG, "click_target_pressed count=" + clicks);
            updateStatus();
        });

        // MODA11Y4 step 3: the service's performGlobalAction(BACK) is a real
        // Back key into this window, so this activity's own finish() runs.
        Button back = (Button) findViewById(R.id.a11y_back);
        back.setOnClickListener(v -> {
            Log.i(TAG, "back_requested");
            A11yProbeService.requestBack();
        });
    }

    @Override
    public boolean onKeyDown(int keyCode, android.view.KeyEvent event) {
        // MODA11Y5: `repeatCount`/`flags` are what `KeyEvent.dispatch` needs to
        // start tracking the press (`repeatCount == 0`, `FLAG_START_TRACKING`
        // set by this very handler); `tracking` must be false on a down.
        Log.i(TAG, "key_down code=" + keyCode + " tracking=" + event.isTracking()
                + " repeat=" + event.getRepeatCount()
                + " flags=" + event.getFlags());
        return super.onKeyDown(keyCode, event);
    }

    @Override
    public boolean onKeyUp(int keyCode, android.view.KeyEvent event) {
        Log.i(TAG, "key_up code=" + keyCode + " tracking=" + event.isTracking());
        return super.onKeyUp(keyCode, event);
    }

    @Override
    public void onBackPressed() {
        Log.i(TAG, "back_pressed");
        super.onBackPressed();
    }

    @Override
    protected void onPause() {
        Log.i(TAG, "activity_paused");
        super.onPause();
    }

    @Override
    protected void onStop() {
        Log.i(TAG, "activity_stopped");
        super.onStop();
    }

    @Override
    protected void onDestroy() {
        Log.i(TAG, "activity_destroyed clicks=" + clicks);
        super.onDestroy();
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
                + " sent=" + sent + first
                + " a11y_click=" + clicks;
        status.setText(text);
        Log.i(TAG, "status " + text);
    }
}
