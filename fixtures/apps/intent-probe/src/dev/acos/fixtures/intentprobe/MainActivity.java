package dev.acos.fixtures.intentprobe;

import android.app.Activity;
import android.content.Intent;
import android.os.Bundle;
import android.util.Log;
import android.view.View;
import android.widget.Button;
import android.widget.TextView;

/**
 * MPROC1 step 2: the app half of the **intent capture target**.
 *
 * <p>Two actions a device would route to some other installed app, in a session
 * that has none:
 *
 * <ul>
 *   <li><b>Share text</b> — {@code ACTION_SEND}, {@code text/plain}, an
 *       {@code EXTRA_TEXT} payload ({@link #ACTION_SHARE}). The device's share
 *       sheet would take it; here the session's capture target records it.</li>
 *   <li><b>Pick image</b> — {@code ACTION_GET_CONTENT}, {@code image/*}, via
 *       {@code startActivityForResult(requestCode=42)} ({@link #ACTION_PICK}).
 *       The device's photo picker would return a content URI; here the agent
 *       scripts that answer and the capture target delivers it.</li>
 * </ul>
 *
 * <p>What the app observes is reported two ways: an {@code ACOSPROBE} line per
 * event (the launch/session logs carry them verbatim) and the {@code status}
 * TextView, so the same facts are readable from logs, tree and pixels.
 *
 * <pre>
 * ACOSPROBE shared action=android.intent.action.SEND type=text/plain text=Shared from ACOS
 * ACOSPROBE pick-request action=android.intent.action.GET_CONTENT type=image/* request=42
 * ACOSPROBE result request=42 result=-1 data=content://… type=image/png
 * </pre>
 */
public class MainActivity extends Activity {
    private static final String TAG = "AcosIntentProbe";

    /** Action the runner sends to press "Share text": {@code am start -a <this>}. */
    public static final String ACTION_SHARE = "dev.acos.fixtures.intentprobe.SHARE";
    /** Action the runner sends to press "Pick image". */
    public static final String ACTION_PICK = "dev.acos.fixtures.intentprobe.PICK";
    /** The request code the pick uses (asserted in the reply). */
    public static final int PICK_REQUEST = 42;

    private TextView status;

    @Override
    protected void onCreate(Bundle savedInstanceState) {
        super.onCreate(savedInstanceState);
        Log.i(TAG, "onCreate");
        setContentView(R.layout.main);
        status = (TextView) findViewById(R.id.status);
        ((Button) findViewById(R.id.share)).setOnClickListener(new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                shareText();
            }
        });
        ((Button) findViewById(R.id.pick)).setOnClickListener(new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                pickImage();
            }
        });
        // The fixture drives both paths itself, once the view exists: the
        // capture target and the scripted result are what is under test, not a
        // button hit test. The buttons above replay them by hand.
        shareText();
        pickImage();
    }

    @Override
    protected void onNewIntent(Intent intent) {
        super.onNewIntent(intent);
        if (intent == null) return;
        if (ACTION_SHARE.equals(intent.getAction())) {
            shareText();
        } else if (ACTION_PICK.equals(intent.getAction())) {
            pickImage();
        }
    }

    /** {@code ACTION_SEND} text/plain with a real EXTRA_TEXT payload. */
    private void shareText() {
        String text = getString(R.string.shared_text);
        Intent send = new Intent(Intent.ACTION_SEND);
        send.setType("text/plain");
        send.putExtra(Intent.EXTRA_TEXT, text);
        send.putExtra(Intent.EXTRA_SUBJECT, "ACOS MPROC1");
        try {
            startActivity(send);
            String line = "ACOSPROBE shared action=" + send.getAction()
                    + " type=" + send.getType() + " text=" + text;
            Log.i(TAG, line);
            System.out.println(line);
            status.setText("shared " + send.getType() + ": " + text);
        } catch (Throwable t) {
            String line = "ACOSPROBE share-failed " + t.getClass().getName()
                    + (t.getMessage() == null ? "" : ": " + t.getMessage());
            Log.i(TAG, line);
            System.out.println(line);
            status.setText(line);
        }
    }

    /** {@code ACTION_GET_CONTENT} image/* through {@code startActivityForResult}. */
    private void pickImage() {
        Intent pick = new Intent(Intent.ACTION_GET_CONTENT);
        pick.setType("image/*");
        pick.addCategory(Intent.CATEGORY_OPENABLE);
        try {
            startActivityForResult(pick, PICK_REQUEST);
            String line = "ACOSPROBE pick-request action=" + pick.getAction()
                    + " type=" + pick.getType() + " request=" + PICK_REQUEST;
            Log.i(TAG, line);
            System.out.println(line);
            status.setText("pick requested: " + pick.getType());
        } catch (Throwable t) {
            String line = "ACOSPROBE pick-failed " + t.getClass().getName()
                    + (t.getMessage() == null ? "" : ": " + t.getMessage());
            Log.i(TAG, line);
            System.out.println(line);
            status.setText(line);
        }
    }

    /**
     * The activity-result round trip: the capture target's scripted answer is
     * delivered as a real {@code ActivityResultItem}, so this is the framework's
     * own callback — nothing here is polled or fabricated.
     */
    @Override
    protected void onActivityResult(int requestCode, int resultCode, Intent data) {
        super.onActivityResult(requestCode, resultCode, data);
        String uri = data == null ? "null" : String.valueOf(data.getDataString());
        String type = data == null ? "null" : String.valueOf(data.getType());
        String line = "ACOSPROBE result request=" + requestCode + " result=" + resultCode
                + " data=" + uri + " type=" + type;
        Log.i(TAG, line);
        System.out.println(line);
        String extra = data == null ? null
                : data.getStringExtra("dev.acos.capture.NOTE");
        status.setText("picked " + resultCode + ": " + uri + "\n" + type
                + (extra == null ? "" : "\n" + extra));
    }
}
