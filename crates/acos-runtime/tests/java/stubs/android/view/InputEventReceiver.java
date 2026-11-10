package android.view;

import android.os.IBinder;
import android.os.Looper;

/**
 * Signature-only stub for {@code android.view.InputEventReceiver}. See
 * {@link InputChannel}.
 */
public abstract class InputEventReceiver {
    public InputEventReceiver(InputChannel inputChannel, Looper looper) {
        throw new UnsupportedOperationException("stub");
    }

    public void onInputEvent(InputEvent event) {
        throw new UnsupportedOperationException("stub");
    }

    public final void finishInputEvent(InputEvent event, boolean handled) {
        throw new UnsupportedOperationException("stub");
    }

    public void dispose() {
        throw new UnsupportedOperationException("stub");
    }

    public IBinder getToken() {
        throw new UnsupportedOperationException("stub");
    }
}
