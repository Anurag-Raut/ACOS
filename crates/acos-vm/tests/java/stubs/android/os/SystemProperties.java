package android.os;

/**
 * Signature-only stub for the {@code @hide} {@code android.os.SystemProperties}.
 *
 * <p>It exists so {@code OsNativesMain.java} can be compiled with the normal SDK
 * {@code android.jar}. Only the signature is used: the compiled stub class is
 * never put into the fixture DEX (the test passes just {@code OsNativesMain.class}
 * to d8), so at runtime the VM resolves the real framework class from
 * {@code framework.jar} and binds the Rust natives.
 *
 * <p>Keep this in sync with the API-34 framework DEX (see
 * {@code natives/os_props.rs} for the exact signatures).
 */
public final class SystemProperties {
    private SystemProperties() {}

    public static String get(String key) {
        throw new UnsupportedOperationException("stub");
    }

    public static String get(String key, String def) {
        throw new UnsupportedOperationException("stub");
    }

    public static int getInt(String key, int def) {
        throw new UnsupportedOperationException("stub");
    }

    public static long getLong(String key, long def) {
        throw new UnsupportedOperationException("stub");
    }

    public static boolean getBoolean(String key, boolean def) {
        throw new UnsupportedOperationException("stub");
    }
}
