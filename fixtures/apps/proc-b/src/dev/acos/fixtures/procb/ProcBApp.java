package dev.acos.fixtures.procb;

import android.app.Application;
import android.util.Log;

/**
 * MPROC7: the second process's `Application`, so a **new process instance** is
 * visible in the guest log.
 *
 * <p>`onCreate` runs once per guest process. A scenario that proves the idle
 * kill needs that distinction: the Activity's `created` line reappears every
 * time an instance is created (even in the same process), so counting it does
 * not prove the process was respawned. Counting `process-start` lines does.
 */
public class ProcBApp extends Application {
    private static final String TAG = "AcosProcB";

    @Override
    public void onCreate() {
        super.onCreate();
        Log.i(TAG, "ACOSPROBE b process-start pid=" + android.os.Process.myPid()
                + " pkg=" + getPackageName());
    }
}
