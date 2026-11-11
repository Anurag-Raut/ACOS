package dev.acos.fixtures.fragmentsnav;

import android.app.Activity;
import android.os.Bundle;
import android.util.Log;
import android.view.View;
import android.widget.Button;
import android.widget.TextView;

/**
 * Second activity reached from {@link MainActivity}; shows the extra it was
 * started with and finishes back to the (fragment) main activity.
 */
public class DetailActivity extends Activity {
    private static final String TAG = "AcosFragments";

    @Override
    protected void onCreate(Bundle savedInstanceState) {
        super.onCreate(savedInstanceState);
        Log.i(TAG, "Detail.onCreate");
        setContentView(R.layout.detail);
        String title = getIntent().getStringExtra("title");
        ((TextView) findViewById(R.id.received)).setText("Received: " + title);
        Log.i(TAG, "Detail.received=" + title);
        ((Button) findViewById(R.id.finish)).setOnClickListener(new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                Log.i(TAG, "Detail.finish");
                finish();
            }
        });
    }

    @Override
    protected void onResume() {
        super.onResume();
        Log.i(TAG, "Detail.onResume");
    }
}
