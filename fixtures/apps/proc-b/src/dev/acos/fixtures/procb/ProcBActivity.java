package dev.acos.fixtures.procb;

import android.app.Activity;
import android.app.PendingIntent;
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
        // MPROC7 step 3: `FLAG_ACTIVITY_SINGLE_TOP` to the resumed top activity.
        // The framework reuses this instance and the host delivers
        // `NewIntentItem`; `onNewIntent` runs (never a second `onCreate`).
        ((Button) findViewById(R.id.again)).setOnClickListener(new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                Intent intent = new Intent(ProcBActivity.this, ProcBActivity.class);
                intent.putExtra(EXTRA_NOTE, "again");
                intent.addFlags(Intent.FLAG_ACTIVITY_SINGLE_TOP);
                Log.i(TAG, "ACOSPROBE b start-again single_top=true");
                startActivity(intent);
            }
        });
        // A second activity of this process asks for a document (the picker's
        // answer has to come back to it).
        ((Button) findViewById(R.id.pick)).setOnClickListener(new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                startActivity(new Intent(ProcBActivity.this, ProcBPickActivity.class));
            }
        });
        String note = getIntent() == null ? null : getIntent().getStringExtra(EXTRA_NOTE);
        String line = "ACOSPROBE b created pkg=" + getPackageName()
                + " note=" + (note == null ? "none" : note)
                + " pid=" + android.os.Process.myPid();
        Log.i(TAG, line);
        status.setText("B\n" + line);

        // PROCUID README §7 item 2: this process is the **second** app of the
        // session (its uid is not the primary package's), so a pending intent
        // created here must answer *this* process as its creator.
        // `PendingIntent.getCreatorUid()`/`getCreatorPackage()` both read
        // `IActivityManager.getInfoForIntentSender`, whose creator uid used to
        // be the fixture uid for every record: the verdict line said BAD for a
        // second app. The comparison is against this process's own uid and
        // package, so the check cannot be satisfied by a constant.
        showPendingIntentCreator();
    }

    /**
     * Create a real {@link PendingIntent} here and render the creator identity
     * the system reports for it, with the verdict of comparing it to this
     * process's own {@code Process.myUid()} / package name.
     */
    private void showPendingIntentCreator() {
        TextView creatorView = (TextView) findViewById(R.id.creator);
        Intent target = new Intent("dev.acos.fixtures.procb.PI");
        target.setPackage(getPackageName());
        int flags = PendingIntent.FLAG_IMMUTABLE | PendingIntent.FLAG_UPDATE_CURRENT;
        PendingIntent pi = PendingIntent.getBroadcast(this, 7, target, flags);
        if (pi == null) {
            String none = "ACOSPROBE b pi none";
            Log.i(TAG, none);
            creatorView.setText(none);
            return;
        }
        int creatorUid = pi.getCreatorUid();
        String creatorPackage = pi.getCreatorPackage();
        boolean ok = creatorUid == android.os.Process.myUid()
                && getPackageName().equals(creatorPackage);
        String line = "ACOSPROBE b pi creator=" + creatorUid + "/" + creatorPackage
                + " mine=" + android.os.Process.myUid() + "/" + getPackageName()
                + " verdict=" + (ok ? "ok" : "BAD");
        Log.i(TAG, line);
        creatorView.setText(line);
    }

    /**
     * MPROC7 step 3: `FLAG_ACTIVITY_SINGLE_TOP` to a resumed activity is an
     * `onNewIntent` (the activity above is the same instance), so the extra the
     * host encoded into `NewIntentItem` must be visible here.
     */
    @Override
    protected void onNewIntent(Intent intent) {
        super.onNewIntent(intent);
        setIntent(intent);
        String note = intent == null ? null : intent.getStringExtra(EXTRA_NOTE);
        Log.i(TAG, "ACOSPROBE b newIntent note=" + (note == null ? "none" : note)
                + " pid=" + android.os.Process.myPid());
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
