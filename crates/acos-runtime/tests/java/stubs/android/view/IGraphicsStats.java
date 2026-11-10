package android.view;

import android.os.IBinder;
import android.os.ParcelFileDescriptor;
import android.os.RemoteException;

/**
 * Signature-only stub for the {@code @hide} {@code android.view.IGraphicsStats}.
 * The real interface + `$Stub`/`$Stub$Proxy` come from the boot-classpath
 * framework DEX at runtime.
 */
public interface IGraphicsStats {
    ParcelFileDescriptor requestBufferForProcess(String packageName,
            IGraphicsStatsCallback callback) throws RemoteException;

    abstract class Stub extends android.os.Binder implements IGraphicsStats {
        public static IGraphicsStats asInterface(IBinder obj) {
            throw new UnsupportedOperationException("stub");
        }
    }
}
