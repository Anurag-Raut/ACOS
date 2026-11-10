package android.view;

import android.os.IBinder;
import android.os.RemoteException;

/**
 * Signature-only stub for the {@code @hide}
 * {@code android.view.IWindowSessionCallback}. The real interface + {@code $Stub}
 * come from the boot-classpath framework DEX at runtime.
 */
public interface IWindowSessionCallback {
    void onAnimatorScaleChanged(float scale) throws RemoteException;

    abstract class Stub extends android.os.Binder implements IWindowSessionCallback {
        public static IWindowSessionCallback asInterface(IBinder obj) {
            throw new UnsupportedOperationException("stub");
        }
    }
}
