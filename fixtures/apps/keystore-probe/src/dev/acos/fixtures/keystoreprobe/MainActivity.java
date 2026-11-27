package dev.acos.fixtures.keystoreprobe;

import android.app.Activity;
import android.os.Bundle;
import android.widget.TextView;

/**
 * The APK vehicle for the K5b probe.
 *
 * <p>It exists so the *app-session* path (`acos launch --engine art` through
 * LaunchMain) can be measured: with no keystore requested, a real app session
 * must keep the honest `AndroidKeyStore not found` failure (item 3), and with
 * the shim installed by LaunchMain the same activity can run the fidelity suite.
 *
 * <p>The mode is read from `ACOS_ART_KEYPROBE_MODE`. The session daemon fixes a
 * guest's environment when the daemon starts, so a *per-call* mode cannot be
 * delivered through `acos launch`; the suite therefore uses the plain
 * `acos-art run` guest for the parameterised modes (see
 * `tools/keystore-k5b/run.sh`) and this activity for the default-off evidence.
 * The default mode is `default_off`, which is exactly what an app session with
 * no keystore configured must observe.
 */
public final class MainActivity extends Activity {
    @Override
    protected void onCreate(Bundle state) {
        super.onCreate(state);
        TextView v = new TextView(this);
        v.setId(android.R.id.text1);
        setContentView(v);
        String mode = System.getenv("ACOS_ART_KEYPROBE_MODE");
        if (mode == null || mode.trim().isEmpty()) {
            mode = java.security.Security.getProvider("AndroidKeyStore") != null
                    ? "launch_fidelity" : "default_off";
        } else {
            mode = mode.trim();
        }
        KeystoreProbe probe = new KeystoreProbe(getFilesDir(), getPackageName());
        probe.run(mode);
        String summary = probe.summaryText(mode);
        v.setText(summary);
        android.util.Log.i("KeystoreProbe", summary);
    }
}
