package dev.acos.fixtures.threadheavy;

import android.app.Activity;
import android.os.Bundle;
import android.util.Log;

/**
 * Thread-heavy fixture (GL1): stands in for a React Native app whose process
 * carries many long-lived non-daemon threads.
 *
 * A Java/ART process does not exit while a non-daemon thread is alive, so an
 * app like this is the counter-example to "wait for the app to end": Android
 * does not wait either — ActivityManagerService SIGKILLs the process after
 * onDestroy. ACOS session destruction must behave the same way: bounded
 * lifecycle grace, then kill the whole session scope, never an unbounded wait
 * for these threads.
 *
 * Observable behaviour:
 *  - TextView id=status shows "ready"
 *  - {@code THREADS} non-daemon threads are started in onCreate and loop
 *    forever (they never observe a stop flag, by design)
 *  - the count of live workers is logged with tag "AcosThreadHeavy"
 */
public class MainActivity extends Activity {
    private static final String TAG = "AcosThreadHeavy";
    /** Deliberately many: Gaten-class apps run dozens of native/Java threads. */
    private static final int THREADS = 12;

    @Override
    protected void onCreate(Bundle savedInstanceState) {
        super.onCreate(savedInstanceState);
        Log.i(TAG, "onCreate");
        setContentView(R.layout.main);
        for (int i = 0; i < THREADS; i++) {
            final int index = i;
            Thread worker = new Thread(new Runnable() {
                @Override
                public void run() {
                    // Non-daemon, never returns: the process cannot exit by
                    // itself once this is running.
                    while (true) {
                        try {
                            Thread.sleep(1000);
                        } catch (InterruptedException ignored) {
                            // Even an interrupt is ignored: the app never ends.
                        }
                    }
                }
            }, "acos-thread-heavy-" + index);
            worker.setDaemon(false);
            worker.start();
        }
        Log.i(TAG, "threads started=" + THREADS);
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
