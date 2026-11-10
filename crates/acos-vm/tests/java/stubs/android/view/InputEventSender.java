package android.view;

import android.os.Looper;

/**
 * Signature-only stub for {@code android.view.InputEventSender}. See
 * {@link InputChannel}.
 */
public class InputEventSender {
    public InputEventSender(InputChannel inputChannel, Looper looper) {
        throw new UnsupportedOperationException("stub");
    }

    public final boolean sendInputEvent(int seq, InputEvent event) {
        throw new UnsupportedOperationException("stub");
    }

    public void onInputEventFinished(int seq, boolean handled) {
        throw new UnsupportedOperationException("stub");
    }

    public void dispose() {
        throw new UnsupportedOperationException("stub");
    }
}
