package com.android.settings;

import android.accessibilityservice.AccessibilityServiceInfo;
import android.app.Activity;
import android.content.ComponentName;
import android.content.Intent;
import android.net.Uri;
import android.os.Bundle;
import android.provider.Settings;
import android.util.Log;
import android.view.View;
import android.view.accessibility.AccessibilityManager;
import android.widget.Button;
import android.widget.CompoundButton;
import android.widget.LinearLayout;
import android.widget.Switch;
import android.widget.TextView;

import java.util.ArrayList;
import java.util.LinkedHashSet;
import java.util.List;
import java.util.Set;

/**
 * Minimal Settings system app ({@code com.android.settings}).
 *
 * Handles {@link Settings#ACTION_SETTINGS}, {@link Settings#ACTION_WIFI_SETTINGS},
 * {@link Settings#ACTION_APPLICATION_DETAILS_SETTINGS} (with {@code package:}
 * data URI), {@link Settings#ACTION_ACCESSIBILITY_SETTINGS} (with live
 * {@code Settings.Secure} toggles), {@link Settings#ACTION_MANAGE_OVERLAY_PERMISSION},
 * and {@link Settings#ACTION_APP_NOTIFICATION_SETTINGS}. Undeclared settings
 * actions continue to throw {@link android.content.ActivityNotFoundException}.
 */
public class SettingsActivity extends Activity {
    private static final String TAG = "AcosSettings";

    private TextView titleView;
    private TextView actionView;
    private TextView detailView;
    private LinearLayout dynamicContainer;

    @Override
    protected void onCreate(Bundle savedInstanceState) {
        super.onCreate(savedInstanceState);
        setContentView(R.layout.settings);
        titleView = (TextView) findViewById(R.id.settings_title);
        actionView = (TextView) findViewById(R.id.settings_action);
        detailView = (TextView) findViewById(R.id.settings_detail);
        dynamicContainer = (LinearLayout) findViewById(R.id.settings_dynamic_container);
        ((Button) findViewById(R.id.settings_back)).setOnClickListener(new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                finish();
            }
        });
        renderIntent(getIntent());
    }

    @Override
    protected void onNewIntent(Intent intent) {
        super.onNewIntent(intent);
        setIntent(intent);
        renderIntent(intent);
    }

    private void renderIntent(Intent intent) {
        String action = intent != null && intent.getAction() != null
                ? intent.getAction()
                : Settings.ACTION_SETTINGS;
        Uri data = intent != null ? intent.getData() : null;
        dynamicContainer.removeAllViews();
        String title;
        String detail;
        if (Settings.ACTION_APPLICATION_DETAILS_SETTINGS.equals(action)) {
            title = "App Info";
            String pkg = data != null ? data.getSchemeSpecificPart() : null;
            detail = "Package: " + (pkg != null && !pkg.isEmpty() ? pkg : "(none)");
        } else if (Settings.ACTION_WIFI_SETTINGS.equals(action)) {
            title = "Wi-Fi Settings";
            detail = "Wi-Fi network settings";
        } else if (Settings.ACTION_ACCESSIBILITY_SETTINGS.equals(action)) {
            title = "Accessibility";
            detail = renderAccessibilityServices();
        } else if (Settings.ACTION_MANAGE_OVERLAY_PERMISSION.equals(action)) {
            title = "Display over other apps";
            String pkg = data != null ? data.getSchemeSpecificPart() : null;
            detail = "Package: " + (pkg != null && !pkg.isEmpty() ? pkg : "(all apps)");
        } else if (Settings.ACTION_APP_NOTIFICATION_SETTINGS.equals(action)) {
            title = "App notifications";
            String pkg = intent != null ? intent.getStringExtra(Settings.EXTRA_APP_PACKAGE) : null;
            detail = "Package: " + (pkg != null && !pkg.isEmpty() ? pkg : "(none)");
        } else {
            title = "Settings";
            detail = "System settings";
        }
        titleView.setText(title);
        actionView.setText(action);
        detailView.setText(detail);
        Log.i(TAG, "ACOSPROBE settings action=" + action
                + " data=" + (data != null ? data.toString() : "none")
                + " title=" + title
                + " detail=" + detail);
    }

    private String renderAccessibilityServices() {
        AccessibilityManager am = (AccessibilityManager) getSystemService(ACCESSIBILITY_SERVICE);
        List<AccessibilityServiceInfo> installed = null;
        if (am != null) {
            try {
                installed = am.getInstalledAccessibilityServiceList();
            } catch (Throwable t) {
                Log.w(TAG, "getInstalledAccessibilityServiceList failed: " + t);
            }
        }
        if (installed == null || installed.isEmpty()) {
            return "No accessibility services installed (or accessibility module is off)";
        }
        Set<String> enabledSet = readEnabledAccessibilityServices();
        int count = 0;
        for (AccessibilityServiceInfo info : installed) {
            if (info == null) {
                continue;
            }
            final String componentId = info.getId();
            if (componentId == null || componentId.isEmpty()) {
                continue;
            }
            String label = componentId;
            if (info.getResolveInfo() != null && getPackageManager() != null) {
                try {
                    CharSequence loaded = info.getResolveInfo().loadLabel(getPackageManager());
                    if (loaded != null && loaded.length() > 0) {
                        label = loaded.toString() + " (" + componentId + ")";
                    }
                } catch (Throwable ignored) {
                }
            }
            Switch toggle = new Switch(this);
            toggle.setText(label);
            toggle.setTextSize(16f);
            toggle.setPadding(0, 16, 0, 16);
            toggle.setChecked(enabledSet.contains(componentId));
            toggle.setOnCheckedChangeListener(new CompoundButton.OnCheckedChangeListener() {
                @Override
                public void onCheckedChanged(CompoundButton buttonView, boolean isChecked) {
                    setAccessibilityServiceEnabled(componentId, isChecked);
                }
            });
            dynamicContainer.addView(toggle);
            count++;
        }
        if (count == 0) {
            return "No accessibility services available";
        }
        return "Toggle an accessibility service to bind or unbind it live";
    }

    private Set<String> readEnabledAccessibilityServices() {
        Set<String> out = new LinkedHashSet<String>();
        String raw = null;
        try {
            raw = Settings.Secure.getString(
                    getContentResolver(), Settings.Secure.ENABLED_ACCESSIBILITY_SERVICES);
        } catch (Throwable t) {
            Log.w(TAG, "read ENABLED_ACCESSIBILITY_SERVICES failed: " + t);
        }
        if (raw != null && !raw.isEmpty()) {
            for (String part : raw.split(":")) {
                if (!part.isEmpty()) {
                    ComponentName cn = ComponentName.unflattenFromString(part);
                    out.add(cn != null ? cn.flattenToShortString() : part);
                }
            }
        }
        return out;
    }

    private void setAccessibilityServiceEnabled(String componentId, boolean enabled) {
        Set<String> current = readEnabledAccessibilityServices();
        if (enabled) {
            current.add(componentId);
        } else {
            current.remove(componentId);
        }
        List<String> list = new ArrayList<String>(current);
        StringBuilder joined = new StringBuilder();
        for (int i = 0; i < list.size(); i++) {
            if (i > 0) {
                joined.append(':');
            }
            joined.append(list.get(i));
        }
        try {
            Settings.Secure.putString(
                    getContentResolver(),
                    Settings.Secure.ENABLED_ACCESSIBILITY_SERVICES,
                    joined.toString());
            Settings.Secure.putInt(
                    getContentResolver(),
                    Settings.Secure.ACCESSIBILITY_ENABLED,
                    list.isEmpty() ? 0 : 1);
            Log.i(TAG, "ACOSPROBE settings_a11y_toggle component=" + componentId
                    + " enabled=" + enabled
                    + " total=" + list.size());
        } catch (Throwable t) {
            Log.w(TAG, "write ENABLED_ACCESSIBILITY_SERVICES failed: " + t);
        }
    }
}
