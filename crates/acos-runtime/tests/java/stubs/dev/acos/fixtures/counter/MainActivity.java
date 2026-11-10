package dev.acos.fixtures.counter;

/**
 * Signature-only compile-time view of the fixture class. It is a `--lib` for
 * d8, never bundled into the test DEX, so the runtime class has to come from
 * the counter APK through {@code PathClassLoader}.
 */
public class MainActivity extends android.app.Activity {
    public static native int probe();
}
