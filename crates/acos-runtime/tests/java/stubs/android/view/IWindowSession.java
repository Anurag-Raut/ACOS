package android.view;

import android.os.IBinder;
import android.os.RemoteException;

/**
 * Signature-only stub for the {@code @hide} {@code android.view.IWindowSession}
 * (the return type of {@code IWindowManager.openSession}). The real interface +
 * {@code $Stub} come from the boot-classpath framework DEX at runtime.
 */
public interface IWindowSession {
    abstract class Stub extends android.os.Binder implements IWindowSession {
        public static IWindowSession asInterface(IBinder obj) {
            throw new UnsupportedOperationException("stub");
        }
    }
}
