package android.view;

/**
 * Signature-only stub shadowing the public {@code android.view.Surface} so the
 * T-29 test can call the {@code @hide} {@code Surface.copyFrom(SurfaceControl)}.
 * The runtime always binds the real framework class.
 */
public class Surface {
    public Surface() {
        throw new UnsupportedOperationException("stub");
    }

    public void copyFrom(SurfaceControl other) {
        throw new UnsupportedOperationException("stub");
    }

    public boolean isValid() {
        throw new UnsupportedOperationException("stub");
    }

    public void release() {
        throw new UnsupportedOperationException("stub");
    }
}
