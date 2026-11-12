package dalvik.system;

/**
 * Z1: signature shadow for ART/libcore's real {@code dalvik.system.ZygoteHooks}.
 *
 * <p>This class is never loaded. It exists only so javac can compile the
 * prototype against the exact hidden-API signatures of the runtime that is
 * actually installed in {@code .cache/art/sysroot} (Android 14 / API 34,
 * extracted from core-libart.jar with {@code dexdump}):
 *
 * <pre>
 *   preFork()V
 *   postForkChild(IZZLjava/lang/String;)V
 *   postForkCommon()V
 *   startZygoteNoThreadCreation()V
 *   stopZygoteNoThreadCreation()V
 *   gcAndFinalize()V
 *   onBeginPreload()V / onEndPreload()V
 * </pre>
 *
 * At run time the app class loader delegates to the boot class loader first, so
 * every call site resolves to the real implementation in libart.so. If the
 * signatures ever drift, the call fails loudly (NoSuchMethodError) instead of
 * silently doing nothing.
 */
public final class ZygoteHooks {
    private ZygoteHooks() {}

    public static void startZygoteNoThreadCreation() {
        throw new IllegalStateException("shadow class: not the real ZygoteHooks");
    }

    public static void stopZygoteNoThreadCreation() {
        throw new IllegalStateException("shadow class: not the real ZygoteHooks");
    }

    public static void preFork() {
        throw new IllegalStateException("shadow class: not the real ZygoteHooks");
    }

    public static void postForkChild(int runtimeFlags, boolean isSystemServer,
                                     boolean isZygote, String instructionSet) {
        throw new IllegalStateException("shadow class: not the real ZygoteHooks");
    }

    public static void postForkCommon() {
        throw new IllegalStateException("shadow class: not the real ZygoteHooks");
    }

    public static void gcAndFinalize() {
        throw new IllegalStateException("shadow class: not the real ZygoteHooks");
    }

    public static void onBeginPreload() {
        throw new IllegalStateException("shadow class: not the real ZygoteHooks");
    }

    public static void onEndPreload() {
        throw new IllegalStateException("shadow class: not the real ZygoteHooks");
    }
}
