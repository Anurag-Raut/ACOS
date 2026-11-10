package dev.acos.fixtures.textinput;

import android.app.Activity;
import android.content.SharedPreferences;
import android.os.Bundle;
import android.util.Log;
import android.view.View;
import android.widget.Button;
import android.widget.EditText;
import android.widget.TextView;

/**
 * Text-input fixture: {@link EditText} + {@link Button} + {@link TextView} with
 * {@link SharedPreferences} persistence. Plain framework APIs only.
 *
 * Observable behaviour:
 *  - the text saved in the previous session is restored into the TextView on
 *    create (tag AcosText)
 *  - pressing Save writes the EditText value to SharedPreferences and updates
 *    the TextView to "Saved: &lt;value&gt;"
 */
public class MainActivity extends Activity {
    private static final String TAG = "AcosText";
    private static final String PREFS = "acos.textinput";
    private static final String KEY = "last";

    private EditText input;
    private TextView saved;

    @Override
    protected void onCreate(Bundle savedInstanceState) {
        super.onCreate(savedInstanceState);
        Log.i(TAG, "onCreate");
        setContentView(R.layout.main);
        input = (EditText) findViewById(R.id.input);
        saved = (TextView) findViewById(R.id.saved);

        SharedPreferences prefs = getSharedPreferences(PREFS, MODE_PRIVATE);
        String last = prefs.getString(KEY, null);
        if (last != null) {
            saved.setText("Saved: " + last);
            Log.i(TAG, "restored=" + last);
        }

        ((Button) findViewById(R.id.save)).setOnClickListener(new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                String value = input.getText().toString();
                getSharedPreferences(PREFS, MODE_PRIVATE)
                        .edit()
                        .putString(KEY, value)
                        .apply();
                saved.setText("Saved: " + value);
                Log.i(TAG, "save value=" + value);
            }
        });
    }

    @Override
    protected void onResume() {
        super.onResume();
        Log.i(TAG, "onResume");
    }
}
