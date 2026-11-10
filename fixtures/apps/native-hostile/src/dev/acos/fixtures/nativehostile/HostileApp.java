package dev.acos.fixtures.nativehostile;

import android.app.Application;
import android.util.Log;

/**
 * Hostile-native fixture: loads a bundled library that attempts actions the
 * ADR-0022 confinement must deny (execve, read /etc/passwd, AF_INET socket,
 * write outside the data dir, ptrace). Each success sets a bit; the confinement
 * must keep the mask at zero while the session survives.
 */
public class HostileApp extends Application {
    // `AcosFixture` so the mask is reported in `acos launch` events.
    private static final String TAG = "AcosFixture";

    private native int hostileProbe();

    @Override
    public void onCreate() {
        super.onCreate();
        System.loadLibrary("hostile");
        Log.i(TAG, "hostile=" + hostileProbe());
    }
}
