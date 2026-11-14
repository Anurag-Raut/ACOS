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
        v.setText("keystore probe");
        setContentView(v);
        String mode = System.getenv("ACOS_ART_KEYPROBE_MODE");
        new KeystoreProbe(getFilesDir(), getPackageName())
                .run(mode == null || mode.trim().isEmpty() ? "default_off" : mode.trim());
    }
}
