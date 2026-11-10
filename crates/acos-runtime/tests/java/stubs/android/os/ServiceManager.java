package android.os;

/**
 * Signature-only stub for the {@code @hide} {@code android.os.ServiceManager}.
 *
 * <p>It exists so the runtime tests can be compiled with the normal SDK
 * {@code android.jar}. Only the signature is used: the compiled stub class is
 * never put into the test DEX (the harness passes it as a {@code --lib} only),
 * so at runtime the VM resolves the real framework class from the boot
 * classpath.
 */
public final class ServiceManager {
    private ServiceManager() {}

    public static IBinder getService(String name) {
        throw new UnsupportedOperationException("stub");
    }

    public static IBinder checkService(String name) {
        throw new UnsupportedOperationException("stub");
    }

    public static void addService(String name, IBinder service) {
        throw new UnsupportedOperationException("stub");
    }
}
