package android.view;

import android.os.RemoteException;

/**
 * Signature-only stub for the {@code @hide}
 * {@code android.view.IGraphicsStatsCallback}. The real interface + `$Stub`
 * come from the boot-classpath framework DEX at runtime.
 */
public interface IGraphicsStatsCallback {
    void onRotateGraphicsStatsBuffer() throws RemoteException;

    abstract class Stub extends android.os.Binder implements IGraphicsStatsCallback {
        public Stub() {}
    }
}
