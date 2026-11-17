package dev.acos.fixtures.procb;

import android.app.Application;
import android.os.Binder;
import android.os.Process;
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
        Log.i(TAG, "ACOSPROBE b process-start pid=" + Process.myPid()
                + " pkg=" + getPackageName());
        // BINDERPID: B's own uid is its package's (a second uid in one session),
        // and the per-uid permission answer must be B's, not A's.
        int uid = Process.myUid();
        String owner;
        try {
            owner = String.valueOf(getPackageManager().getNameForUid(uid));
        } catch (Throwable t) {
            owner = "EX:" + t.getClass().getSimpleName();
        }
        String perm;
        try {
            perm = String.valueOf(checkCallingPermission("android.permission.INTERNET"));
        } catch (Throwable t) {
            perm = "EX:" + t.getClass().getSimpleName();
        }
        String self;
        try {
            self = String.valueOf(checkSelfPermission("android.permission.INTERNET"));
        } catch (Throwable t) {
            self = "EX:" + t.getClass().getSimpleName();
        }
        Log.i(TAG, "ACOSPROBE b identity pkg=" + getPackageName()
                + " uid=" + uid + " pid=" + Process.myPid()
                + " calling-uid=" + Binder.getCallingUid()
                + " calling-pid=" + Binder.getCallingPid()
                + " uid-pkg=" + owner
                + " internet=" + perm + " self=" + self);
    }
}
