package dev.acos.fixtures.twoactivity;

import android.app.Activity;
import android.content.res.Configuration;
import android.os.Bundle;
import android.util.Log;
import android.view.View;
import android.widget.Button;
import android.widget.TextView;

/**
 * Second activity of the two-activity fixture: reads the "name" Intent extra,
 * keeps a counter in instance state, and is also rotation-free.
 * Plain framework APIs only.
 *
 * Observable behaviour:
 *  - TextView id=greeting shows the extra; TextView id=counter shows a counter
 *  - Button id=inc increments the counter
 *  - Button id=back finishes and returns to MainActivity
 *  - a rotation does not recreate this activity (onConfigurationChanged only)
 *  - lifecycle/actions log with tag "AcosTwo", prefixed "Detail."
 */
public class DetailActivity extends Activity {
    private static final String TAG = "AcosTwo";
    private static final String KEY_COUNT = "count";

    private int count = 0;
    private TextView counter;
    private int configChanges = 0;

    @Override
    protected void onCreate(Bundle savedInstanceState) {
        super.onCreate(savedInstanceState);
        Log.i(TAG, "Detail.onCreate");
        setContentView(R.layout.detail);
        String name = getIntent().getStringExtra("name");
        if (name == null) {
            name = "";
        }
        int attempt = getIntent().getIntExtra("attempt", 0);
        Log.i(TAG, "Detail.extra name=" + name + " attempt=" + attempt);
        ((TextView) findViewById(R.id.greeting)).setText("Hello, " + name);
        counter = (TextView) findViewById(R.id.counter);
        if (savedInstanceState != null) {
            count = savedInstanceState.getInt(KEY_COUNT, 0);
            Log.i(TAG, "Detail.restored count=" + count);
        }
        showCount();
        ((Button) findViewById(R.id.inc)).setOnClickListener(new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                count++;
                showCount();
                Log.i(TAG, "Detail.inc count=" + count);
            }
        });
        ((Button) findViewById(R.id.back)).setOnClickListener(new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                Log.i(TAG, "Detail.back");
                finish();
            }
        });
    }

    private void showCount() {
        counter.setText("count=" + count);
    }

    @Override
    public void onConfigurationChanged(Configuration newConfig) {
        super.onConfigurationChanged(newConfig);
        configChanges++;
        Log.i(TAG, "Detail.onConfigurationChanged count=" + configChanges
                + " orientation=" + newConfig.orientation);
    }

    @Override
    protected void onSaveInstanceState(Bundle outState) {
        super.onSaveInstanceState(outState);
        outState.putInt(KEY_COUNT, count);
        Log.i(TAG, "Detail.onSaveInstanceState count=" + count);
    }

    @Override
    protected void onStart() {
        super.onStart();
        Log.i(TAG, "Detail.onStart");
    }

    @Override
    protected void onResume() {
        super.onResume();
        Log.i(TAG, "Detail.onResume");
    }

    @Override
    protected void onPause() {
        super.onPause();
        Log.i(TAG, "Detail.onPause");
    }

    @Override
    protected void onStop() {
        super.onStop();
        Log.i(TAG, "Detail.onStop");
    }

    @Override
    protected void onDestroy() {
        super.onDestroy();
        Log.i(TAG, "Detail.onDestroy");
    }
}
