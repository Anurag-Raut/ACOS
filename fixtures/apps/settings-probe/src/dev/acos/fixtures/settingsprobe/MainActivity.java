package dev.acos.fixtures.settingsprobe;

import android.app.Activity;
import android.content.ActivityNotFoundException;
import android.content.Intent;
import android.net.Uri;
import android.os.Bundle;
import android.provider.Settings;
import android.util.Log;
import android.view.View;
import android.widget.Button;
import android.widget.TextView;

/**
 * LAUNCHER2 probe fixture: fires real {@link Settings} intents and reports
 * either launch or honest {@link ActivityNotFoundException}.
 */
public class MainActivity extends Activity {
    private static final String TAG = "AcosSettingsProbe";

    private TextView status;

    @Override
    protected void onCreate(Bundle savedInstanceState) {
        super.onCreate(savedInstanceState);
        setContentView(R.layout.main);
        status = (TextView) findViewById(R.id.status);

        ((Button) findViewById(R.id.btn_settings)).setOnClickListener(new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                launchSettings(new Intent(Settings.ACTION_SETTINGS));
            }
        });
        ((Button) findViewById(R.id.btn_app_details)).setOnClickListener(new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                Intent i = new Intent(Settings.ACTION_APPLICATION_DETAILS_SETTINGS);
                i.setData(Uri.fromParts("package", getPackageName(), null));
                launchSettings(i);
            }
        });
        ((Button) findViewById(R.id.btn_wifi)).setOnClickListener(new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                launchSettings(new Intent(Settings.ACTION_WIFI_SETTINGS));
            }
        });
        ((Button) findViewById(R.id.btn_location)).setOnClickListener(new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                launchSettings(new Intent(Settings.ACTION_LOCATION_SOURCE_SETTINGS));
            }
        });
    }

    @Override
    protected void onResume() {
        super.onResume();
        Log.i(TAG, "ACOSPROBE settings-probe resumed");
    }

    private void launchSettings(Intent intent) {
        String action = intent.getAction();
        try {
            startActivity(intent);
            Log.i(TAG, "ACOSPROBE settings-probe started action=" + action);
            status.setText("started: " + action);
        } catch (ActivityNotFoundException e) {
            Log.i(TAG, "ACOSPROBE settings-probe not-found action=" + action
                    + " err=" + e.getClass().getName());
            status.setText("ActivityNotFoundException: " + action);
        }
    }
}
