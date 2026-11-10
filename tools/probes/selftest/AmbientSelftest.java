package dev.acos.probes.selftest;

import android.content.Context;
import android.content.pm.PackageManager;
import android.os.Build;
import android.os.HandlerThread;
import android.os.SystemProperties;
import android.provider.Settings;

/**
 * Known-answer fixture for the ambient-state probes.  Every call site below is
 * constructed so the expected value is obvious from the source:
 *
 *   propDefaults      -> ro.selftest.key / fallback (SystemProperties.get String,String)
 *   propInt           -> debug.selftest.int / 42    (SystemProperties.getInt)
 *   propStaticField   -> debug.selftest.static      (sget-object + get(String))
 *   propDynamic       -> ro.selftest.* (dynamic)    (StringBuilder concat)
 *   settingLiteral    -> Settings.Global.getInt("adb_enabled") (inlined constant)
 *   settingViaField   -> Settings.Global.getInt(Keys.SETTINGS_KEY) (sget-object)
 *   feature           -> android.hardware.touchscreen
 *   service           -> Context.WINDOW_SERVICE
 *   thread            -> HandlerThread("selftest.thread")
 *   sdkGate           -> Build.VERSION.SDK_INT >= 33
 *   sdkGateCodes      -> Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU
 */
public final class AmbientSelftest {
    static String DYNAMIC_PREFIX = "ro.selftest.";
    static String STATIC_KEY = "debug.selftest.static";

    private AmbientSelftest() {}

    public static String propDefaults() {
        return SystemProperties.get("ro.selftest.key", "fallback");
    }

    public static int propInt() {
        return SystemProperties.getInt("debug.selftest.int", 42);
    }

    public static String propStaticField() {
        return SystemProperties.get(STATIC_KEY);
    }

    public static String propDynamic(String suffix) {
        return SystemProperties.get(DYNAMIC_PREFIX + suffix);
    }

    public static long propLong() {
        return SystemProperties.getLong("debug.selftest.long", 7L);
    }

    public static boolean propBool() {
        return SystemProperties.getBoolean("debug.selftest.bool", true);
    }

    public static int settingLiteral() {
        return Settings.Global.getInt(null, Settings.Global.ADB_ENABLED, 0);
    }

    public static int settingViaField() {
        return Settings.Global.getInt(null, Keys.SETTINGS_KEY, 0);
    }

    public static boolean feature(PackageManager pm) {
        return pm.hasSystemFeature("android.hardware.touchscreen");
    }

    public static Object service(Context ctx) {
        return ctx.getSystemService(Context.WINDOW_SERVICE);
    }

    public static String serviceName(Context ctx) {
        return ctx.getSystemServiceName(Context.class);
    }

    public static Thread thread() {
        return new HandlerThread("selftest.thread");
    }

    public static boolean sdkGate() {
        return Build.VERSION.SDK_INT >= 33;
    }

    public static boolean sdkGateCodes() {
        return Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU;
    }
}
