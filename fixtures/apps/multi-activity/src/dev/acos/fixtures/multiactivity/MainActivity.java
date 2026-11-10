package dev.acos.fixtures.multiactivity;

import android.app.Activity;
import android.content.Intent;
import android.os.Bundle;
import android.util.Log;
import android.view.View;
import android.widget.Button;
import android.widget.EditText;

/**
 * Multi-activity fixture, launcher activity. Plain framework APIs only.
 *
 * Observable behaviour:
 *  - EditText id=message + Button id=send starts SecondActivity with an extra
 *  - the typed message is preserved across a configuration change via
 *    onSaveInstanceState
 *  - Back from SecondActivity returns to this activity (back stack)
 *  - lifecycle is logged with tag "AcosMulti", prefixed "Main."
 */
public class MainActivity extends Activity {
    private static final String TAG = "AcosMulti";
    private static final String KEY_MESSAGE = "message";

    private EditText message;

    @Override
    protected void onCreate(Bundle savedInstanceState) {
        super.onCreate(savedInstanceState);
        Log.i(TAG, "Main.onCreate");
        setContentView(R.layout.main);
        message = (EditText) findViewById(R.id.message);
        if (savedInstanceState != null) {
            String restored = savedInstanceState.getString(KEY_MESSAGE, "");
            message.setText(restored);
            Log.i(TAG, "Main.restored message=" + restored);
        }
        ((Button) findViewById(R.id.send)).setOnClickListener(new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                String text = message.getText().toString();
                Log.i(TAG, "Main.send message=" + text);
                Intent intent = new Intent(MainActivity.this, SecondActivity.class);
                intent.putExtra("message", text);
                startActivity(intent);
            }
        });
    }

    @Override
    protected void onStart() {
        super.onStart();
        Log.i(TAG, "Main.onStart");
    }

    @Override
    protected void onResume() {
        super.onResume();
        Log.i(TAG, "Main.onResume");
    }

    @Override
    protected void onPause() {
        super.onPause();
        Log.i(TAG, "Main.onPause");
    }

    @Override
    protected void onStop() {
        super.onStop();
        Log.i(TAG, "Main.onStop");
    }

    @Override
    protected void onDestroy() {
        super.onDestroy();
        Log.i(TAG, "Main.onDestroy");
    }

    @Override
    protected void onSaveInstanceState(Bundle outState) {
        super.onSaveInstanceState(outState);
        String text = message == null ? "" : message.getText().toString();
        outState.putString(KEY_MESSAGE, text);
        Log.i(TAG, "Main.onSaveInstanceState message=" + text);
    }
}
