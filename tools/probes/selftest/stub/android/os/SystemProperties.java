// Minimal stub of the hidden android.os.SystemProperties API so the selftest
// fixture can be compiled with the public SDK android.jar (which strips @hide
// classes).  d8 links the call sites exactly as the real framework does.
package android.os;

public final class SystemProperties {
    private SystemProperties() {}

    public static String get(String key) {
        return "";
    }

    public static String get(String key, String def) {
        return def;
    }

    public static int getInt(String key, int def) {
        return def;
    }

    public static long getLong(String key, long def) {
        return def;
    }

    public static boolean getBoolean(String key, boolean def) {
        return def;
    }

    public static void addChangeCallback(Runnable callback) {
    }
}
