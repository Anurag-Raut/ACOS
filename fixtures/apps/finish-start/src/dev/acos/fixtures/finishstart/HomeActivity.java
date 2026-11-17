package dev.acos.fixtures.finishstart;

import android.app.Activity;
import android.os.Bundle;
import android.util.Log;

/** ROT8b fixture: the Activity the handoff lands on. */
public class HomeActivity extends Activity {
    private static final String TAG = "AcosFinishStart";

    @Override
    protected void onCreate(Bundle savedInstanceState) {
        super.onCreate(savedInstanceState);
        Log.i(TAG, "Home.onCreate");
        setContentView(R.layout.home);
    }

    @Override
    protected void onResume() {
        super.onResume();
        Log.i(TAG, "Home.onResume");
    }
}
