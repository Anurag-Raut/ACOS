package android.content;

import android.os.Bundle;
import android.os.IBinder;
import android.os.RemoteException;

/**
 * Signature-only stub for the {@code @hide} {@code android.content.IIntentSender}
 * (`IIntentSender.aidl`: one oneway `send`). The real interface + {@code $Stub}
 * come from the boot-classpath framework DEX at runtime; the MODNOTIF4 Java
 * case uses it to build a `PendingIntent` without a system service.
 */
public interface IIntentSender {
    void send(int code, Intent intent, String resolvedType, IBinder whitelistToken,
            IIntentReceiver finishedReceiver, String requiredPermission, Bundle options)
            throws RemoteException;

    abstract class Stub extends android.os.Binder implements IIntentSender {
        public static IIntentSender asInterface(IBinder obj) {
            throw new UnsupportedOperationException("stub");
        }
    }
}
