package dev.acos.fixtures.helloviews;

import android.app.Activity;
import android.os.Bundle;
import android.util.Log;
import android.view.View;
import android.widget.Button;
import android.widget.EditText;
import android.widget.TextView;

/**
 * Minimal View-based activity used as a real-APK fixture for ACOS.
 *
 * Observable behaviour:
 *  - shows title "Hello ACOS" (TextView id=title)
 *  - shows "clicks: 0" (TextView id=result) initially
 *  - clicking Button id=btn with text in EditText id=input sets result to
 *    "Hello, <input>! clicks: N" and increments N
 *  - logs lifecycle callbacks with tag "AcosFixture"
 *  - counter is saved/restored via onSaveInstanceState
 */
public class MainActivity extends Activity {
    private static final String TAG = "AcosFixture";
    private static final String KEY_CLICKS = "clicks";

    private TextView result;
    private EditText input;
    private int clicks = 0;

    @Override
    protected void onCreate(Bundle savedInstanceState) {
        super.onCreate(savedInstanceState);
        Log.i(TAG, "onCreate");
        setContentView(R.layout.main);
        input = (EditText) findViewById(R.id.input);
        result = (TextView) findViewById(R.id.result);
        Button btn = (Button) findViewById(R.id.btn);
        if (savedInstanceState != null) {
            clicks = savedInstanceState.getInt(KEY_CLICKS, 0);
            result.setText("clicks: " + clicks);
        }
        btn.setOnClickListener(new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                String name = input.getText().toString();
                clicks++;
                result.setText("Hello, " + name + "! clicks: " + clicks);
                Log.i(TAG, "greet name=" + name + " clicks=" + clicks);
            }
        });
    }

    @Override
    protected void onStart() {
        super.onStart();
        Log.i(TAG, "onStart");
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
    protected void onStop() {
        super.onStop();
        Log.i(TAG, "onStop");
    }

    @Override
    protected void onDestroy() {
        super.onDestroy();
        Log.i(TAG, "onDestroy");
    }

    @Override
    protected void onSaveInstanceState(Bundle outState) {
        super.onSaveInstanceState(outState);
        outState.putInt(KEY_CLICKS, clicks);
        Log.i(TAG, "onSaveInstanceState clicks=" + clicks);
    }
}
