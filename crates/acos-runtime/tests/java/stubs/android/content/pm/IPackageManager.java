package android.content.pm;

import android.os.IBinder;
import android.os.RemoteException;

/**
 * Signature-only stub for the {@code @hide}
 * {@code android.content.pm.IPackageManager}. The real interface + `$Stub`
 * come from the boot-classpath framework DEX at runtime.
 */
public interface IPackageManager {
    String[] getPackagesForUid(int uid) throws RemoteException;

    abstract class Stub extends android.os.Binder implements IPackageManager {
        public static IPackageManager asInterface(IBinder obj) {
            throw new UnsupportedOperationException("stub");
        }
    }
}
