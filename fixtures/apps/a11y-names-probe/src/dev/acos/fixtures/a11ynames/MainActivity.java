package dev.acos.fixtures.a11ynames;

import android.app.Activity;
import android.os.Bundle;
import android.view.View;
import android.view.accessibility.AccessibilityNodeInfo;

/**
 * PARITYFIX F1 fixture: one view per React Native accessibility shape, so a
 * scenario can pin the accessible name ACOS publishes for each to what
 * {@link AccessibilityNodeInfo} (the object {@code uiautomator dump} prints)
 * reports.
 *
 * <ul>
 *   <li>{@code token}: EditText, hint + contentDescription, password input.
 *   <li>{@code search}: EditText, hint + contentDescription.
 *   <li>{@code recent}: TextView, text only.
 *   <li>{@code both}: TextView, text and its own contentDescription.
 *   <li>{@code card}: clickable container with a contentDescription and a
 *       text child.
 *   <li>{@code delegate_tab}: a TextView whose contentDescription exists only
 *       on the node info, set by an accessibility delegate, exactly how
 *       {@code ReactAccessibilityDelegate} composes a name. {@code
 *       View.getContentDescription()} is null here.
 * </ul>
 */
public class MainActivity extends Activity {
    @Override
    protected void onCreate(Bundle savedInstanceState) {
        super.onCreate(savedInstanceState);
        setContentView(R.layout.main);
        View tab = findViewById(R.id.delegate_tab);
        tab.setAccessibilityDelegate(new View.AccessibilityDelegate() {
            @Override
            public void onInitializeAccessibilityNodeInfo(View host, AccessibilityNodeInfo info) {
                super.onInitializeAccessibilityNodeInfo(host, info);
                info.setContentDescription("Analytics, tab 2 of 4");
            }
        });
    }
}
