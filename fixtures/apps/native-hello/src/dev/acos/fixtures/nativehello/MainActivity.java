package dev.acos.fixtures.nativehello;

import android.app.Activity;
import android.os.Bundle;

/**
 * Trivial launcher activity; the native evidence is produced in
 * {@link NativeHelloApp#onCreate} so it runs even while the activity lifecycle
 * is still being completed.
 */
public class MainActivity extends Activity {
    @Override
    protected void onCreate(Bundle savedInstanceState) {
        super.onCreate(savedInstanceState);
    }
}
