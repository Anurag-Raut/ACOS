package dev.acos.fixtures.procb;

import android.app.Activity;
import android.content.Intent;
import android.os.Bundle;
import android.util.Log;
import android.widget.TextView;

import java.io.BufferedReader;
import java.io.InputStream;
import java.io.InputStreamReader;

/**
 * A second activity of the **second process** that asks for a document.
 *
 * <p>The scripted answer must reach this activity, not the process's first one
 * and not the primary process:
 *
 * <pre>
 * ACOSPROBE b pick asked request=41
 * ACOSPROBE b pick result=-1 text=...
 * </pre>
 */
public class ProcBPickActivity extends Activity {
    private static final String TAG = "AcosProcB";
    private static final int REQUEST_PICK = 41;

    private TextView picked;

    @Override
    protected void onCreate(Bundle savedInstanceState) {
        super.onCreate(savedInstanceState);
        picked = new TextView(this);
        picked.setId(R.id.picked);
        picked.setText("waiting");
        setContentView(picked);
        Intent pick = new Intent(Intent.ACTION_OPEN_DOCUMENT);
        pick.addCategory(Intent.CATEGORY_OPENABLE);
        pick.setType("text/plain");
        startActivityForResult(pick, REQUEST_PICK);
        Log.i(TAG, "ACOSPROBE b pick asked request=" + REQUEST_PICK);
    }

    @Override
    protected void onActivityResult(int requestCode, int resultCode, Intent data) {
        super.onActivityResult(requestCode, resultCode, data);
        String text = "none";
        if (requestCode == REQUEST_PICK && data != null && data.getData() != null) {
            try (InputStream in = getContentResolver().openInputStream(data.getData());
                    BufferedReader reader = new BufferedReader(new InputStreamReader(in))) {
                text = reader.readLine();
            } catch (Exception e) {
                text = "error " + e;
            }
        }
        String line = "ACOSPROBE b pick result=" + resultCode + " text=" + text;
        Log.i(TAG, line);
        picked.setText(line);
    }
}
