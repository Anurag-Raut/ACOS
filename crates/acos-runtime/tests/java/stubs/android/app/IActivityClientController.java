package android.app;

import android.os.IBinder;
import android.os.RemoteException;

/**
 * Signature-only stub for the {@code @hide}
 * {@code android.app.IActivityClientController}. The real interface + `$Stub`
 * come from the boot-classpath framework DEX at runtime.
 */
public interface IActivityClientController {
    void activityResumed(IBinder token, boolean focused) throws RemoteException;

    abstract class Stub extends android.os.Binder implements IActivityClientController {
        public static IActivityClientController asInterface(IBinder obj) {
            throw new UnsupportedOperationException("stub");
        }
    }
}
