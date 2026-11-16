package android.content;

import android.os.Bundle;
import android.os.IBinder;
import android.os.RemoteException;

/**
 * Signature-only stub for the {@code @hide} {@code android.content.IIntentReceiver}
 * (the completed-receiver argument of `IIntentSender.send`). The real interface
 * comes from the boot-classpath framework DEX at runtime.
 */
public interface IIntentReceiver {
    void performReceive(Intent intent, int resultCode, String data, Bundle extras,
            boolean ordered, boolean sticky, int sendingUser) throws RemoteException;

    abstract class Stub extends android.os.Binder implements IIntentReceiver {
        public static IIntentReceiver asInterface(IBinder obj) {
            throw new UnsupportedOperationException("stub");
        }
    }
}
