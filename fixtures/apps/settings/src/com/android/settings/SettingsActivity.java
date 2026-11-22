package com.android.settings;

import android.app.Activity;
import android.content.Intent;
import android.net.Uri;
import android.os.Bundle;
import android.provider.Settings;
import android.util.Log;
import android.view.View;
import android.widget.Button;
import android.widget.TextView;

/**
 * LAUNCHER2: minimal Settings stub system app ({@code com.android.settings}).
 *
 * Handles {@link Settings#ACTION_SETTINGS}, {@link Settings#ACTION_WIFI_SETTINGS},
 * and {@link Settings#ACTION_APPLICATION_DETAILS_SETTINGS} (with {@code package:}
 * data URI). Unhandled settings actions are intentionally not declared so they
 * continue to throw {@link android.content.ActivityNotFoundException}.
 */
public class SettingsActivity extends Activity {
    private static final String TAG = "AcosSettings";

    private TextView titleView;
    private TextView actionView;
    private TextView detailView;

    @Override
    protected void onCreate(Bundle savedInstanceState) {
        super.onCreate(savedInstanceState);
        setContentView(R.layout.settings);
        titleView = (TextView) findViewById(R.id.settings_title);
        actionView = (TextView) findViewById(R.id.settings_action);
        detailView = (TextView) findViewById(R.id.settings_detail);
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
        String title;
        String detail;
        if (Settings.ACTION_APPLICATION_DETAILS_SETTINGS.equals(action)) {
            title = "App Info";
            String pkg = data != null ? data.getSchemeSpecificPart() : null;
            detail = "Package: " + (pkg != null && !pkg.isEmpty() ? pkg : "(none)");
        } else if (Settings.ACTION_WIFI_SETTINGS.equals(action)) {
            title = "Wi-Fi Settings";
            detail = "Wi-Fi network settings";
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
}
