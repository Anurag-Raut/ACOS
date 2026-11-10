package android.os;

/**
 * Signature-only stub for {@code android.os.SystemClock}.
 *
 * <p>The SDK's {@code android.jar} omits the {@code @hide} members ACOS has to
 * supply ({@code uptimeNanos}, {@code currentTimeMicro}). As with
 * {@link SystemProperties}, only the signature is used: the stub class is a
 * {@code --lib} for d8 and is never packaged into the fixture DEX, so the VM
 * binds the real framework class and the Rust natives.
 */
public final class SystemClock {
    private SystemClock() {}

    public static long uptimeMillis() {
        throw new UnsupportedOperationException("stub");
    }

    public static long uptimeNanos() {
        throw new UnsupportedOperationException("stub");
    }

    public static long elapsedRealtime() {
        throw new UnsupportedOperationException("stub");
    }

    public static long elapsedRealtimeNanos() {
        throw new UnsupportedOperationException("stub");
    }

    public static long currentThreadTimeMillis() {
        throw new UnsupportedOperationException("stub");
    }

    public static long currentTimeMicro() {
        throw new UnsupportedOperationException("stub");
    }
}
