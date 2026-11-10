import android.graphics.Paint;
import android.graphics.RectF;
import android.graphics.Typeface;
import android.graphics.fonts.Font;
import android.graphics.fonts.FontFamily;
import android.graphics.fonts.FontStyle;
import android.graphics.fonts.FontVariationAxis;
import java.io.File;

/**
 * Java-level acceptance for the ACOS font natives (W23-A).
 *
 * Runs on the real Android 14 boot classpath (`--corelib real`). It drives the
 * exact bytecode `Typeface.<clinit>` + `loadPreinstalledSystemFontMap` use:
 *
 *  - `SystemFonts.createFontFamily` -> `Font$Builder` / `FontFamily$Builder`
 *    natives over a real `FileChannel.map` of `/system/fonts/Roboto-Regular.ttf`;
 *  - `Typeface.createFromFamilies` -> `nativeCreateFromArray`;
 *  - `Typeface.setSystemFontMap` -> `nativeForceSetStaticFinalField`, which is
 *    what makes `Typeface.DEFAULT` non-null;
 *  - `Paint.measureText` through the bold `sans-serif` typeface resolved from
 *    the real system font map.
 *
 * `loadPreinstalledSystemFontMap` is hidden from the SDK stub, so it is invoked
 * reflectively against the real boot classpath (the same reflection ACOS
 * already supports for `Unsafe.theUnsafe`).
 */
public class FontNativesMain {
    public static void main(String[] args) throws Exception {
        // Compiled against `tests/java/font_stubs` because the SDK stub hides
        // this method; at runtime the real boot-classpath Typeface is loaded.
        Typeface.loadPreinstalledSystemFontMap();

        Typeface def = Typeface.DEFAULT;
        System.out.println("default.present=" + (def != null));
        System.out.println("default.family=" + def.getSystemFontFamilyName());
        System.out.println("default.style=" + def.getStyle());

        Typeface bold = Typeface.create("sans-serif", Typeface.BOLD);
        System.out.println("bold.present=" + (bold != null));
        System.out.println("bold.style=" + bold.getStyle());

        Paint p = new Paint();
        p.setTextSize(24f);
        p.setTypeface(bold);
        float at24 = p.measureText("Hello");
        System.out.println("measure.24=" + Float.toString(at24));
        p.setTextSize(48f);
        float at48 = p.measureText("Hello");
        System.out.println("measure.48=" + Float.toString(at48));

        // Direct Font / FontFamily native surface.
        File file = new File("/system/fonts/Roboto-Regular.ttf");
        Font font = new Font.Builder(file)
                .setWeight(FontStyle.FONT_WEIGHT_BOLD)
                .setSlant(FontStyle.FONT_SLANT_UPRIGHT)
                .build();
        FontFamily family = new FontFamily.Builder(font).build();
        System.out.println("family.size=" + family.getSize());
        Font f0 = family.getFont(0);
        System.out.println("font.weight=" + f0.getStyle().getWeight());
        System.out.println("font.index=" + f0.getTtcIndex());
        System.out.println("font.file=" + f0.getFile().getName());
        System.out.println("font.axes=" + f0.getAxes().length);

        Paint.FontMetrics metrics = new Paint.FontMetrics();
        f0.getMetrics(p, metrics);
        System.out.println("font.ascent=" + Float.toString(metrics.ascent));
        System.out.println("font.descent=" + Float.toString(metrics.descent));

        RectF bounds = new RectF();
        float advance = f0.getGlyphBounds(0, p, bounds);
        System.out.println("glyph.advance=" + Float.toString(advance));
        System.out.println("glyph.rect=" + (bounds.right >= bounds.left)
                + "," + (bounds.bottom >= bounds.top));

        System.out.println("done");
    }
}
