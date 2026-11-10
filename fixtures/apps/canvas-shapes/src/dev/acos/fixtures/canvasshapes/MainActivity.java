package dev.acos.fixtures.canvasshapes;

import android.app.Activity;
import android.os.Bundle;
import android.util.Log;
import android.widget.TextView;

/**
 * Canvas fixture host activity. Plain framework APIs only.
 *
 * The interesting work is in {@link ShapeView}; this activity only inflates the
 * layout and keeps a status line so a scenario has a text anchor.
 * Lifecycle logs with tag "AcosCanvas".
 */
public class MainActivity extends Activity {
    private static final String TAG = "AcosCanvas";

    @Override
    protected void onCreate(Bundle savedInstanceState) {
        super.onCreate(savedInstanceState);
        Log.i(TAG, "onCreate");
        setContentView(R.layout.main);
        TextView status = (TextView) findViewById(R.id.canvas_status);
        status.setText("canvas ready");
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
}
