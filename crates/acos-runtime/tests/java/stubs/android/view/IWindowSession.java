package android.view;

import android.graphics.Rect;
import android.os.Bundle;
import android.os.IBinder;
import android.os.RemoteException;
import android.util.MergedConfiguration;
import android.window.ClientWindowFrames;

/**
 * Signature-only stub for the {@code @hide} {@code android.view.IWindowSession}
 * (the return type of {@code IWindowManager.openSession}). The real interface +
 * {@code $Stub} come from the boot-classpath framework DEX at runtime. The
 * signatures match the real {@code Stub$Proxy} exactly (`InsetsSourceControl$Array`
 * is the parcelable wrapper, not a bare array).
 */
public interface IWindowSession {
    int addToDisplayAsUser(IWindow window, WindowManager.LayoutParams attrs, int userId,
            int viewVisibility, int displayId, int requestedVisibleTypes,
            InputChannel outInputChannel, InsetsState outInsetsState,
            InsetsSourceControl.Array outActiveControls, android.graphics.Rect outFrame,
            float[] outFloat);

    int relayout(IWindow window, WindowManager.LayoutParams attrs, int requestedWidth,
            int requestedHeight, int viewVisibility, int flags, int frameNumber,
            int lastSyncSeqId, ClientWindowFrames outFrames,
            MergedConfiguration outMergedConfiguration, SurfaceControl outSurfaceControl,
            InsetsState outInsetsState, InsetsSourceControl.Array outActiveControls,
            Bundle outBundle);

    void remove(IWindow window);

    abstract class Stub extends android.os.Binder implements IWindowSession {
        public static IWindowSession asInterface(IBinder obj) {
            throw new UnsupportedOperationException("stub");
        }
    }
}
