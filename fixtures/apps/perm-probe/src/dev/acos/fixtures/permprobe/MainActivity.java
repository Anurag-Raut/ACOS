package dev.acos.fixtures.permprobe;

import android.app.Activity;
import android.content.pm.PackageManager;
import android.os.Bundle;
import android.util.Log;
import android.view.View;
import android.widget.Button;
import android.widget.TextView;

/**
 * SYSUI1 probe: the framework's real permission-request path end to end.
 *
 * <p>`requestPermissions` → `PackageManager.buildRequestPermissionsIntent` →
 * `startActivityForResult` at `com.android.permissioncontroller` → the guest
 * system app's `GrantPermissionsActivity` → `ActivityResultItem` back →
 * `onRequestPermissionsResult`, then `checkSelfPermission` answers from the one
 * runtime-permission table.
 */
public class MainActivity extends Activity {
    private static final String TAG = "AcosPermProbe";
    private static final int REQUEST_CODE = 7;
    private static final String[] PERMISSIONS = {
        android.Manifest.permission.CAMERA,
        android.Manifest.permission.ACCESS_FINE_LOCATION,
    };

    private TextView stateView;
    private TextView resultView;

    @Override
    protected void onCreate(Bundle savedInstanceState) {
        super.onCreate(savedInstanceState);
        Log.i(TAG, "created");
        setContentView(R.layout.main);
        stateView = (TextView) findViewById(R.id.state);
        resultView = (TextView) findViewById(R.id.result);
        resultView.setText("result: none");
        showState("before");
        Button request = (Button) findViewById(R.id.request);
        request.setOnClickListener(new View.OnClickListener() {
            @Override
            public void onClick(View view) {
                request.setText("requested");
                // A visible change before the request: the tap's settle needs a
                // fresh guest sample, and a request that starts and finishes
                // without drawing (every permission already decided) would
                // otherwise leave the screen identical.
                resultView.setText("result: asking");
                Log.i(TAG, "requesting code=" + REQUEST_CODE);
                requestPermissions(PERMISSIONS, REQUEST_CODE);
            }
        });
    }

    @Override
    public void onRequestPermissionsResult(
            int requestCode, String[] permissions, int[] grantResults) {
        StringBuilder text = new StringBuilder();
        for (int i = 0; i < permissions.length; i++) {
            if (i > 0) {
                text.append(' ');
            }
            text.append(shortName(permissions[i])).append('=')
                    .append(i < grantResults.length ? grantResults[i] : Integer.MIN_VALUE);
        }
        String shape = "result request=" + requestCode + " n=" + permissions.length
                + " m=" + grantResults.length + " " + text;
        Log.i(TAG, shape);
        resultView.setText(shape);
        showState("after");
    }

    private void showState(String when) {
        StringBuilder text = new StringBuilder();
        for (String permission : PERMISSIONS) {
            if (text.length() > 0) {
                text.append(' ');
            }
            text.append(shortName(permission)).append('=')
                    .append(checkSelfPermission(permission));
        }
        String shape = "state " + when + ": " + text;
        Log.i(TAG, shape);
        stateView.setText(shape);
    }

    private static String shortName(String permission) {
        int dot = permission.lastIndexOf('.');
        String name = dot < 0 ? permission : permission.substring(dot + 1);
        return name.replace("ACCESS_FINE_LOCATION", "LOCATION");
    }
}
