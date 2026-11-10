package android.graphics;

/**
 * Compile-time-only stub for the hidden `Typeface` API.
 *
 * `FontNativesMain` needs to call the package-private/public-hidden
 * `loadPreinstalledSystemFontMap()`, which the SDK `android.jar` does not
 * expose. At runtime the real boot-classpath `android.graphics.Typeface` is
 * loaded (the stub is passed to `d8`/`javac` as a library only, never emitted
 * into the app DEX), so the call resolves to the real method and driver.
 *
 * Mirrors the `tests/java/native_mem_stubs` approach for `sun.misc.Unsafe`.
 */
public class Typeface {
    public static final int NORMAL = 0;
    public static final int BOLD = 1;
    public static final int ITALIC = 2;
    public static final int BOLD_ITALIC = 3;

    public static final Typeface DEFAULT = null;
    public static final Typeface DEFAULT_BOLD = null;
    public static final Typeface SANS_SERIF = null;
    public static final Typeface SERIF = null;
    public static final Typeface MONOSPACE = null;

    public static void loadPreinstalledSystemFontMap() {
        throw new RuntimeException("stub");
    }

    public static Typeface create(String familyName, int style) {
        throw new RuntimeException("stub");
    }

    public static Typeface create(Typeface family, int style) {
        throw new RuntimeException("stub");
    }

    public int getStyle() {
        throw new RuntimeException("stub");
    }

    public final String getSystemFontFamilyName() {
        throw new RuntimeException("stub");
    }
}
