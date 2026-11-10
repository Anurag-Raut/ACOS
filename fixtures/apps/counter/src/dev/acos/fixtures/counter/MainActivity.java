package dev.acos.fixtures.counter;

import android.app.Activity;
import android.os.Bundle;
import android.util.Log;
import android.view.View;
import android.widget.Button;
import android.widget.TextView;

/**
 * Counter fixture: a Button + TextView whose state survives rotation through
 * onSaveInstanceState. Plain framework APIs only (no AndroidX).
 *
 * Observable behaviour:
 *  - TextView id=count starts at "0"
 *  - Button id=inc increments and shows the new value
 *  - Button id=reset sets it back to "0"
 *  - the count is restored after a configuration change (rotation)
 *  - lifecycle/actions are logged with tag "AcosCounter"
 */
public class MainActivity extends Activity {
    private static final String TAG = "AcosCounter";
    private static final String KEY_COUNT = "count";

    private TextView countView;
    private int count = 0;

    @Override
    protected void onCreate(Bundle savedInstanceState) {
        super.onCreate(savedInstanceState);
        Log.i(TAG, "onCreate");
        setContentView(R.layout.main);
        countView = (TextView) findViewById(R.id.count);
        if (savedInstanceState != null) {
            count = savedInstanceState.getInt(KEY_COUNT, 0);
            Log.i(TAG, "restored count=" + count);
        }
        countView.setText(String.valueOf(count));
        ((Button) findViewById(R.id.inc)).setOnClickListener(new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                count++;
                countView.setText(String.valueOf(count));
                Log.i(TAG, "increment value=" + count);
            }
        });
        ((Button) findViewById(R.id.reset)).setOnClickListener(new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                count = 0;
                countView.setText(String.valueOf(count));
                Log.i(TAG, "reset value=" + count);
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
        outState.putInt(KEY_COUNT, count);
        Log.i(TAG, "onSaveInstanceState count=" + count);
    }
}
