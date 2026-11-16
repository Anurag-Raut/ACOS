package dev.acos.fixtures.notifpermprobe;

import android.app.Activity;
import android.content.Intent;
import android.os.Bundle;
import android.util.Log;
import android.widget.TextView;

/**
 * The notification's PendingIntent target.
 *
 * <p>It exists so the sender has a real explicit component to start. It is not
 * asserted by the scenarios: a system-originated second launch is delivered to
 * the guest as a {@code LaunchActivityItem}, which is the MPROC delivery class;
 * MODNOTIF5 asserts the sender's {@code {start_result}} instead. If the guest
 * does draw it, the screen says which intent arrived.
 */
public class TargetActivity extends Activity {
    private static final String TAG = "AcosNotifProbe";

    @Override
    protected void onCreate(Bundle savedInstanceState) {
        super.onCreate(savedInstanceState);
        Intent intent = getIntent();
        String from = intent == null ? "-" : String.valueOf(intent.getStringExtra("from"));
        String action = intent == null ? "-" : String.valueOf(intent.getAction());
        Log.i(TAG, "TargetActivity opened from=" + from + " action=" + action);
        TextView view = new TextView(this);
        view.setText("target opened from=" + from + " action=" + action);
        setContentView(view);
    }
}
