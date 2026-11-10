package android.view;

import android.os.IBinder;

/**
 * Signature-only stub for the {@code @hide} {@code android.view.IWindow}. The
 * real interface + {@code $Stub} come from the boot-classpath framework DEX at
 * runtime; this stub exists only so the test case compiles.
 */
public interface IWindow {
    void dispatchAppVisibility(boolean visible);
    void windowFocusChanged(boolean focused);
    void resized(android.window.ClientWindowFrames frames, boolean reportDraw,
            android.util.MergedConfiguration mergedConfiguration,
            InsetsState insetsState, boolean forceLayout,
            boolean alwaysConsumeSystemBars, int displayId, int syncSeqId,
            boolean dragResizing);
    void insetsControlChanged(InsetsState insetsState, InsetsSourceControl[] activeControls);
    void closeSystemDialogs(String reason);

    abstract class Stub extends android.os.Binder implements IWindow {
        public static IWindow asInterface(IBinder obj) {
            throw new UnsupportedOperationException("stub");
        }

        public void dispatchAppVisibility(boolean visible) {
        }

        public void windowFocusChanged(boolean focused) {
        }

        public void resized(android.window.ClientWindowFrames frames, boolean reportDraw,
                android.util.MergedConfiguration mergedConfiguration,
                InsetsState insetsState, boolean forceLayout,
                boolean alwaysConsumeSystemBars, int displayId, int syncSeqId,
                boolean dragResizing) {
        }

        public void insetsControlChanged(InsetsState insetsState,
                InsetsSourceControl[] activeControls) {
        }

        public void closeSystemDialogs(String reason) {
        }
    }
}
