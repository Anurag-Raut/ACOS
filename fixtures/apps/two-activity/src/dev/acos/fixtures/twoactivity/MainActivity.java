package dev.acos.fixtures.twoactivity;

import android.app.Activity;
import android.content.Intent;
import android.content.res.Configuration;
import android.os.Bundle;
import android.util.Log;
import android.view.View;
import android.widget.Button;
import android.widget.EditText;
import android.widget.TextView;

/**
 * Launcher activity for the two-activity / rotation-free lifecycle fixture.
 * Plain framework APIs only (no AndroidX).
 *
 * Observable behaviour:
 *  - Button id=go starts {@link DetailActivity} with a "name" Intent extra
 *  - because the manifest declares configChanges=orientation|screenSize, a
 *    rotation calls {@link #onConfigurationChanged} and does NOT destroy or
 *    recreate this activity (no second onCreate)
 *  - the typed name survives a rotation through instance state
 *  - lifecycle/actions log with tag "AcosTwo", prefixed "Main."
 */
public class MainActivity extends Activity {
    private static final String TAG = "AcosTwo";
    private static final String KEY_NAME = "name";

    private EditText name;
    private TextView state;
    private int configChanges = 0;

    @Override
    protected void onCreate(Bundle savedInstanceState) {
        super.onCreate(savedInstanceState);
        Log.i(TAG, "Main.onCreate");
        setContentView(R.layout.main);
        name = (EditText) findViewById(R.id.name);
        state = (TextView) findViewById(R.id.main_state);
        if (savedInstanceState != null) {
            String restored = savedInstanceState.getString(KEY_NAME, "");
            name.setText(restored);
            Log.i(TAG, "Main.restored name=" + restored);
        }
        ((Button) findViewById(R.id.go)).setOnClickListener(new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                String text = name.getText().toString();
                Log.i(TAG, "Main.go name=" + text);
                Intent intent = new Intent(MainActivity.this, DetailActivity.class);
                intent.putExtra("name", text);
                intent.putExtra("attempt", 1);
                startActivity(intent);
            }
        });
    }

    @Override
    public void onConfigurationChanged(Configuration newConfig) {
        super.onConfigurationChanged(newConfig);
        configChanges++;
        Log.i(TAG, "Main.onConfigurationChanged count=" + configChanges
                + " orientation=" + newConfig.orientation);
        state.setText("config changes=" + configChanges);
    }

    @Override
    protected void onSaveInstanceState(Bundle outState) {
        super.onSaveInstanceState(outState);
        String text = name == null ? "" : name.getText().toString();
        outState.putString(KEY_NAME, text);
        Log.i(TAG, "Main.onSaveInstanceState name=" + text);
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
}
