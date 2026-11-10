package android.window;

import android.graphics.Rect;

/**
 * Signature-only stub for the {@code @hide}
 * {@code android.window.ClientWindowFrames}. The runtime binds the real class.
 */
public class ClientWindowFrames {
    public Rect frame;
    public Rect displayFrame;
    public Rect parentFrame;
    public Rect attachedFrame;
    public boolean isParentFrameClippedByDisplayCutout;
    public float compatScale;

    public ClientWindowFrames() {
        throw new UnsupportedOperationException("stub");
    }
}
