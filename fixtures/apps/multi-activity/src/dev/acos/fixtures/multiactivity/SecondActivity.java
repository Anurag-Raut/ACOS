package dev.acos.fixtures.multiactivity;

import android.app.Activity;
import android.os.Bundle;
import android.util.Log;
import android.view.View;
import android.widget.Button;
import android.widget.TextView;

/**
 * Second activity of the multi-activity fixture: reads the "message" extra and
 * shows it. Lifecycle is logged with tag "AcosMulti", prefixed "Second.".
 */
public class SecondActivity extends Activity {
    private static final String TAG = "AcosMulti";

    @Override
    protected void onCreate(Bundle savedInstanceState) {
        super.onCreate(savedInstanceState);
        Log.i(TAG, "Second.onCreate");
        setContentView(R.layout.second);
        String message = getIntent().getStringExtra("message");
        if (message == null) {
            message = "";
        }
        Log.i(TAG, "Second.received=" + message);
        TextView received = (TextView) findViewById(R.id.received);
        received.setText("Received: " + message);
        ((Button) findViewById(R.id.finish)).setOnClickListener(new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                Log.i(TAG, "Second.finish");
                finish();
            }
        });
    }

    @Override
    protected void onStart() {
        super.onStart();
        Log.i(TAG, "Second.onStart");
    }

    @Override
    protected void onResume() {
        super.onResume();
        Log.i(TAG, "Second.onResume");
    }

    @Override
    protected void onPause() {
        super.onPause();
        Log.i(TAG, "Second.onPause");
    }

    @Override
    protected void onStop() {
        super.onStop();
        Log.i(TAG, "Second.onStop");
    }

    @Override
    protected void onDestroy() {
        super.onDestroy();
        Log.i(TAG, "Second.onDestroy");
    }
}
