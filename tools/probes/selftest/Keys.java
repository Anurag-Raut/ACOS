package dev.acos.probes.selftest;

/** A non-inlined static String so the probe must resolve an sget-object field. */
public final class Keys {
    public static String SETTINGS_KEY = "selftest_setting_key";

    private Keys() {}
}
