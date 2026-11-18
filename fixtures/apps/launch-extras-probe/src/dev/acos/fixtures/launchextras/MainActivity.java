package dev.acos.fixtures.launchextras;

import android.app.Activity;
import android.content.Intent;
import android.os.Bundle;
import android.util.Log;
import android.widget.TextView;

/**
 * LAUNCHEXTRAS: the app half of the launch-Intent probe.
 *
 * <p>The scenario declares an action, a data URI and typed extras on its launch
 * step; the ART launcher builds the real `LaunchActivityItem` Intent from them
 * (ADR-0029 amendment) and this Activity reports exactly what its
 * `getIntent()` returned — one {@code ACOSPROBE} line per fact, plus a status
 * TextView so the same facts are readable from the tree.
 *
 * <p>Nothing is invented here: a value the Intent does not carry reads as the
 * framework's own default ({@code null}, {@code 0}, {@code false}), which is
 * exactly what a missing extra looks like on a device.
 *
 * <pre>
 * ACOSPROBE launch-extras action=dev.acos.fixtures.launchextras.ACTION
 * ACOSPROBE launch-extras data=acos://launch/extras?source=scenario
 * ACOSPROBE launch-extras string=hello
 * ACOSPROBE launch-extras int=42
 * ACOSPROBE launch-extras long=5000000000
 * ACOSPROBE launch-extras long_small=7
 * ACOSPROBE launch-extras boolean=true
 * ACOSPROBE launch-extras float=1.5
 * ACOSPROBE launch-extras double=0.25
 * ACOSPROBE launch-extras string_array=[alpha, beta]
 * </pre>
 */
public class MainActivity extends Activity {
    private static final String TAG = "AcosLaunchExtras";

    @Override
    protected void onCreate(Bundle savedInstanceState) {
        super.onCreate(savedInstanceState);
        setContentView(R.layout.main);
        Intent intent = getIntent();
        String action = intent == null ? null : intent.getAction();
        String data = intent == null ? null : intent.getDataString();
        Bundle extras = intent == null ? null : intent.getExtras();
        probe("action", String.valueOf(action));
        probe("data", String.valueOf(data));
        if (extras != null) {
            probe("string", String.valueOf(extras.getString("probe_string")));
            probe("int", String.valueOf(extras.getInt("probe_int")));
            probe("long", String.valueOf(extras.getLong("probe_long")));
            // Explicit `{type: long, value: 7}`: a value that FITS an int, so
            // only the type tag on the wire can carry "long" (a plain number
            // is inferred as an int, and `getLong` would read 0 from it).
            probe("long_small", String.valueOf(extras.getLong("probe_long_small")));
            probe("boolean", String.valueOf(extras.getBoolean("probe_boolean")));
            probe("float", String.valueOf(extras.getFloat("probe_float")));
            probe("double", String.valueOf(extras.getDouble("probe_double")));
            String[] array = extras.getStringArray("probe_string_array");
            String rendered = array == null ? "null" : "[" + String.join(", ", array) + "]";
            probe("string_array", rendered);
        } else {
            probe("extras", "null");
        }
        TextView status = (TextView) findViewById(R.id.status);
        status.setText("action=" + action + " data=" + data);
    }

    private void probe(String key, String value) {
        String line = "ACOSPROBE launch-extras " + key + "=" + value;
        Log.i(TAG, line);
        System.out.println(line);
    }
}
