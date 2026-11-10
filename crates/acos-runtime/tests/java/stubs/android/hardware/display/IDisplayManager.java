package android.hardware.display;

import android.hardware.OverlayProperties;
import android.os.IBinder;
import android.os.RemoteException;
import android.view.DisplayInfo;

/**
 * Signature-only stub for the {@code @hide}
 * {@code android.hardware.display.IDisplayManager}. The real interface and
 * {@code $Stub} come from the boot-classpath framework DEX at runtime.
 */
public interface IDisplayManager {
    DisplayInfo getDisplayInfo(int displayId) throws RemoteException;

    OverlayProperties getOverlaySupport() throws RemoteException;

    abstract class Stub extends android.os.Binder implements IDisplayManager {
        public static IDisplayManager asInterface(IBinder obj) {
            throw new UnsupportedOperationException("stub");
        }
    }
}
