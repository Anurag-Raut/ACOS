package dev.acos.r2;

/**
 * Loads a library with {@code System.loadLibrary} <em>from</em> a classloader
 * whose library search path is the APK's {@code lib/x86_64} directory.
 *
 * {@code System.loadLibrary} is caller-classloader-scoped: ART picks the
 * libnativeloader namespace of the classloader that loaded the calling class.
 * Our harness class lives in the {@code -cp} dex, so calling
 * {@code System.loadLibrary} from it would use the *harness* namespace and never
 * see the APK's libraries (measured: "dlopen failed: library libjsi.so not
 * found"). R2Main therefore loads this class through a DexClassLoader created
 * with {@code librarySearchPath = <apk>/lib/x86_64}, which is the same
 * arrangement an app's own secondary classloader has (RN's SoLoader does the
 * same thing for libraries shipped in the APK).
 */
public final class RnLoader {
    private RnLoader() {}

    public static void load(String name) {
        System.loadLibrary(name);
    }
}
