package android.content;

import android.os.IBinder;

/**
 * Signature-only stub for the {@code @hide}
 * {@code android.content.IOnPrimaryClipChangedListener}. The real interface +
 * {@code $Stub} come from the boot-classpath framework DEX at runtime; the
 * transport delivers {@code dispatchPrimaryClipChanged()} to the app stub
 * oneway.
 */
public interface IOnPrimaryClipChangedListener {
    void dispatchPrimaryClipChanged();

    abstract class Stub extends android.os.Binder implements IOnPrimaryClipChangedListener {
        public static IOnPrimaryClipChangedListener asInterface(IBinder obj) {
            throw new UnsupportedOperationException("stub");
        }
    }
}
