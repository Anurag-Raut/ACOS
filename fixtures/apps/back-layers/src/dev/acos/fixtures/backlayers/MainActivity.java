package dev.acos.fixtures.backlayers;

import android.app.Activity;
import android.app.AlertDialog;
import android.content.DialogInterface;
import android.os.Bundle;
import android.util.Log;
import android.view.KeyEvent;
import android.view.View;
import android.view.inputmethod.InputMethodManager;
import android.widget.Button;
import android.widget.EditText;
import android.widget.TextView;

/**
 * Back-layers fixture: whose layer consumes a Back press.
 *
 * It puts the two layers that sit above an Activity in front of the framework's
 * own {@code onBackPressed} and records, on the app side, which one took the
 * press:
 *
 * <ul>
 *   <li>an {@link AlertDialog} (a separate window with its own back handling:
 *       {@code Dialog.onBackPressed} cancels and dismisses it);</li>
 *   <li>an {@link EditText} that raises the soft input (the IME is the layer
 *       above the view tree; a phone hides the keyboard first).</li>
 * </ul>
 *
 * Observable behaviour (tag {@code AcosBack}):
 * <ul>
 *   <li>{@code dialog_shown} / {@code dialog_dismissed} / {@code dialog_ok} — the
 *       dialog window's own lifecycle;</li>
 *   <li>{@code dispatchKeyBack action=…} — the Activity's window received the
 *       Back key (the dialog and the IME did not consume it);</li>
 *   <li>{@code onBackPressed} — the framework's own default handling ran;</li>
 *   <li>{@code ime accepting=… active=…} at focus changes and at each Back the
 *       activity sees, so an assertion can tell whether the framework thinks the
 *       soft input is up;</li>
 *   <li>{@code onPause}/{@code onResume}/{@code onDestroy} — the Activity was
 *       never left, or the task finished after the last Back.</li>
 * </ul>
 */
public class MainActivity extends Activity {
    private static final String TAG = "AcosBack";

    private EditText input;
    private TextView status;

    @Override
    protected void onCreate(Bundle savedInstanceState) {
        super.onCreate(savedInstanceState);
        Log.i(TAG, "onCreate");
        setContentView(R.layout.main);
        status = (TextView) findViewById(R.id.status);
        input = (EditText) findViewById(R.id.input);

        ((Button) findViewById(R.id.dialog)).setOnClickListener(new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                showDialog();
            }
        });
        input.setOnFocusChangeListener(new View.OnFocusChangeListener() {
            @Override
            public void onFocusChange(View v, boolean hasFocus) {
                Log.i(TAG, "input_focus=" + hasFocus + " " + imeState());
            }
        });
    }

    private String imeState() {
        InputMethodManager imm =
                (InputMethodManager) getSystemService(INPUT_METHOD_SERVICE);
        if (imm == null) {
            return "ime unavailable";
        }
        return "ime accepting=" + imm.isAcceptingText() + " active=" + imm.isActive(input);
    }

    private void showDialog() {
        AlertDialog dialog = new AlertDialog.Builder(this)
                .setTitle(R.string.dialog_title)
                .setMessage(R.string.dialog_message)
                .setPositiveButton("OK", new DialogInterface.OnClickListener() {
                    @Override
                    public void onClick(DialogInterface d, int which) {
                        status.setText("Dialog confirmed");
                        Log.i(TAG, "dialog_ok which=" + which);
                    }
                })
                .setNegativeButton("Cancel", null)
                .create();
        dialog.setOnDismissListener(new DialogInterface.OnDismissListener() {
            @Override
            public void onDismiss(DialogInterface d) {
                Log.i(TAG, "dialog_dismissed");
            }
        });
        dialog.show();
        Log.i(TAG, "dialog_shown");
    }

    @Override
    public boolean dispatchKeyEvent(KeyEvent event) {
        if (event.getKeyCode() == KeyEvent.KEYCODE_BACK) {
            Log.i(TAG, "dispatchKeyBack action=" + event.getAction() + " " + imeState());
        }
        return super.dispatchKeyEvent(event);
    }

    @Override
    public void onBackPressed() {
        Log.i(TAG, "onBackPressed");
        super.onBackPressed();
    }

    @Override
    protected void onResume() {
        super.onResume();
        Log.i(TAG, "onResume");
    }

    @Override
    protected void onPause() {
        super.onPause();
        Log.i(TAG, "onPause");
    }

    @Override
    protected void onDestroy() {
        super.onDestroy();
        Log.i(TAG, "onDestroy");
    }
}
