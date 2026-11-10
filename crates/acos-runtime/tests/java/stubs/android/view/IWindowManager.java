package android.view;

import android.os.IBinder;
import android.os.RemoteException;

/**
 * Signature-only stub for the {@code @hide} {@code android.view.IWindowManager}.
 * The real interface + {@code $Stub} come from the boot-classpath framework DEX
 * at runtime; only the signatures ACOS's {@code WmsMain} exercises are declared.
 */
public interface IWindowManager {
    boolean useBLAST() throws RemoteException;

    float getCurrentAnimatorScale() throws RemoteException;

    int getBaseDisplayDensity(int displayId) throws RemoteException;

    int getInitialDisplayDensity(int displayId) throws RemoteException;

    boolean hasNavigationBar(int displayId) throws RemoteException;

    boolean isInTouchMode(int displayId) throws RemoteException;

    void setInTouchMode(boolean inTouchMode, int displayId) throws RemoteException;

    IWindowSession openSession(IWindowSessionCallback callback) throws RemoteException;

    abstract class Stub extends android.os.Binder implements IWindowManager {
        public static IWindowManager asInterface(IBinder obj) {
            throw new UnsupportedOperationException("stub");
        }
    }
}
