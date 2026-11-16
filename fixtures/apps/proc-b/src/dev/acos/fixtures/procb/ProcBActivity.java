package dev.acos.fixtures.procb;

import android.app.Activity;
import android.content.Intent;
import android.os.Bundle;
import android.util.Log;
import android.view.View;
import android.widget.Button;
import android.widget.TextView;

/**
 * MPROC4 step 3c: the activity of the **second process**.
 *
 * <p>`dev.acos.fixtures.proca` starts this activity with
 * `startActivityForResult`. On a device the activity runs in
 * `dev.acos.fixtures.procb`'s process, which is created for it; here that means
 * the launcher spawned a second guest for this package. The activity reports its
 * own lifecycle (so the host can prove the window on top belongs to process B)
 * and answers with a result when the driver taps {@code Finish}:
 *
 * <pre>
 * ACOSPROBE b resumed pkg=dev.acos.fixtures.procb note=hello
 * ACOSPROBE b finish code=-1 data=from-b
 * </pre>
 *
 * <p>Every line is the app's own log; nothing is synthesised.
 */
public class ProcBActivity extends Activity {
    private static final String TAG = "AcosProcB";

    /** The note proc-a sends with the launch intent. */
    public static final String EXTRA_NOTE = "note";
    /** The string proc-a expects back in {@link #RESULT_OK}. */
    public static final String RESULT_DATA = "from-b";

    private TextView status;

    @Override
    protected void onCreate(Bundle savedInstanceState) {
        super.onCreate(savedInstanceState);
        Log.i(TAG, "onCreate");
        setContentView(R.layout.main);
        status = (TextView) findViewById(R.id.status);
        ((Button) findViewById(R.id.finish)).setOnClickListener(new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                finishWithResult();
            }
        });
        String note = getIntent() == null ? null : getIntent().getStringExtra(EXTRA_NOTE);
        String line = "ACOSPROBE b created pkg=" + getPackageName()
                + " note=" + (note == null ? "none" : note)
                + " pid=" + android.os.Process.myPid();
        Log.i(TAG, line);
        status.setText("B\n" + line);
    }

    @Override
    protected void onResume() {
        super.onResume();
        Log.i(TAG, "onResume");
        Log.i(TAG, "ACOSPROBE b resumed pkg=" + getPackageName());
    }

    @Override
    protected void onPause() {
        super.onPause();
        Log.i(TAG, "ACOSPROBE b paused");
    }

    @Override
    protected void onStop() {
        super.onStop();
        Log.i(TAG, "ACOSPROBE b stopped");
    }

    /** `setResult` + `finish`: the answer proc-a's `onActivityResult` waits for. */
    private void finishWithResult() {
        Intent data = new Intent();
        data.putExtra("data", RESULT_DATA);
        setResult(RESULT_OK, data);
        Log.i(TAG, "ACOSPROBE b finish code=" + RESULT_OK + " data=" + RESULT_DATA);
        finish();
    }
}
