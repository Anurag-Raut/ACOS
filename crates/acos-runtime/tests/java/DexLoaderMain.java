import dalvik.system.PathClassLoader;

/**
 * Acceptance for the app class loader: build a real {@link PathClassLoader}
 * over the staged counter APK and resolve an app class through the real
 * framework bytecode (`BaseDexClassLoader.findClass` -> `DexPathList` ->
 * `DexFile.defineClassNative`).
 *
 * <p>Then invoke a static method declared by that app class. The call site is
 * compiled against a signature-only stub (a d8 `--lib`, never bundled), so the
 * body that executes can only be the APK's {@code MainActivity.probe()},
 * proving both the resolution and the code came from the APK dex.
 */
public final class DexLoaderMain {
    static final String APK = "/data/user/0/dev.acos.fixtures.counter/base.apk";
    static final String ACTIVITY = "dev.acos.fixtures.counter.MainActivity";

    static void check(boolean cond, String msg) {
        if (!cond) {
            throw new AssertionError(msg);
        }
    }

    public static void main(String[] args) throws Exception {
        ClassLoader parent = ClassLoader.getSystemClassLoader();
        PathClassLoader loader = new PathClassLoader(APK, parent);
        Class<?> activity = loader.loadClass(ACTIVITY);
        check(activity != null, "loadClass returned null");
        check(ACTIVITY.equals(activity.getName()), "wrong class: " + activity.getName());
        // The call site is compiled against the signature-only stub; the body
        // that runs is the one in the APK dex, resolved through the loaded
        // class. 4242 is the fixture's probe() value.
        int value = dev.acos.fixtures.counter.MainActivity.probe();
        System.out.println("loaded=" + activity.getName());
        System.out.println("probe=" + value);
    }
}
