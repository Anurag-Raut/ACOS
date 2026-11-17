package dev.acos.fixtures.finishstart;

import android.app.Activity;
import android.content.Intent;
import android.os.Bundle;
import android.util.Log;
import android.view.View;
import android.widget.Button;

/**
 * ROT8b fixture: the "logout / splash handoff" shape. Tapping Go runs
 * finish() and startActivity(HomeActivity) in the same click handler, so the
 * Splash Activity is destroyed while Home's launch is still queued. Lifecycle
 * is logged under tag "AcosFinishStart", prefixed "Splash.".
 */
public class SplashActivity extends Activity {
    private static final String TAG = "AcosFinishStart";

    @Override
    protected void onCreate(Bundle savedInstanceState) {
        super.onCreate(savedInstanceState);
        Log.i(TAG, "Splash.onCreate");
        setContentView(R.layout.splash);
        ((Button) findViewById(R.id.go)).setOnClickListener(new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                Log.i(TAG, "Splash.go");
                finish();
                startActivity(new Intent(SplashActivity.this, HomeActivity.class));
            }
        });
    }

    @Override
    protected void onResume() {
        super.onResume();
        Log.i(TAG, "Splash.onResume");
    }

    @Override
    protected void onDestroy() {
        super.onDestroy();
        Log.i(TAG, "Splash.onDestroy");
    }
}
