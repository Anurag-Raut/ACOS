package android.view;

import android.os.IBinder;
import android.os.Parcel;

/**
 * Signature-only stub for {@code android.view.InputChannel}.
 *
 * <p>The SDK's {@code android.jar} omits this {@code @hide} class. Only the
 * signatures used by the T-36 fixtures are declared: the stub is a {@code --lib}
 * for d8 and is never packaged into the fixture DEX, so the VM binds the real
 * framework class and the Rust natives.
 */
public class InputChannel {
    public InputChannel() {
        throw new UnsupportedOperationException("stub");
    }

    public static InputChannel[] openInputChannelPair(String name) {
        throw new UnsupportedOperationException("stub");
    }

    public void dispose() {
        throw new UnsupportedOperationException("stub");
    }

    public String getName() {
        throw new UnsupportedOperationException("stub");
    }

    public IBinder getToken() {
        throw new UnsupportedOperationException("stub");
    }

    public InputChannel dup() {
        throw new UnsupportedOperationException("stub");
    }

    public void readFromParcel(Parcel in) {
        throw new UnsupportedOperationException("stub");
    }

    public void writeToParcel(Parcel out, int flags) {
        throw new UnsupportedOperationException("stub");
    }
}
