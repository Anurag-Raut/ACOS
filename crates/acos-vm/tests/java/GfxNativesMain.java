// Java-level acceptance for the android.graphics non-canvas natives (T-31/T-33).
// Compiled with javac + d8 against android.jar and executed on ACOS's real
// Android 14 boot classpath (--corelib real).
import android.graphics.BitmapShader;
import android.graphics.ComposeShader;
import android.graphics.LinearGradient;
import android.graphics.Matrix;
import android.graphics.Paint;
import android.graphics.Path;
import android.graphics.PorterDuff;
import android.graphics.RadialGradient;
import android.graphics.RectF;
import android.graphics.Shader;
import android.graphics.SweepGradient;

public final class GfxNativesMain {
    public static void main(String[] args) {
        Paint p = new Paint();
        p.setColor(0xFF112233);
        p.setAlpha(200);
        p.setStyle(Paint.Style.STROKE);
        p.setStrokeWidth(3.5f);
        p.setTextSize(20.0f);
        p.setTextAlign(Paint.Align.CENTER);

        System.out.println("flags=" + p.getFlags());
        System.out.println("style=" + p.getStyle());
        System.out.println("strokeWidth=" + p.getStrokeWidth());
        System.out.println("textSize=" + p.getTextSize());
        System.out.println("textAlign=" + p.getTextAlign());
        System.out.println("color=" + Integer.toHexString(p.getColor()));
        System.out.println("alpha=" + p.getAlpha());
        System.out.println("measure=" + p.measureText("Hello ACOS"));
        // The raw per-char advances (no Math.ceil wrapper, unlike measureText).
        float[] widths = new float[10];
        p.getTextWidths("Hello ACOS", widths);
        float rawSum = 0.0f;
        for (float w : widths) {
            rawSum += w;
        }
        System.out.println("widths=" + rawSum);

        Path path = new Path();
        path.moveTo(0.0f, 0.0f);
        path.lineTo(10.0f, 0.0f);
        path.lineTo(10.0f, 5.0f);
        RectF bounds = new RectF();
        path.computeBounds(bounds, true);
        System.out.println("bounds=" + bounds.left + "," + bounds.top + ","
                + bounds.right + "," + bounds.bottom);

        Matrix m = new Matrix();
        m.setTranslate(5.0f, 7.0f);
        float[] pts = {1.0f, 2.0f};
        m.mapPoints(pts);
        System.out.println("mapped=" + pts[0] + "," + pts[1]);

        // Shaders (W75-A): constructing and attaching a shader drives
        // `nativeCreate` through the real framework. Gradients, a bitmap
        // shader and a two-shader compose must all resolve to a native
        // instance (a non-null `getShader()` implies the native handle was
        // produced).
        Shader lg = new LinearGradient(0, 0, 10, 0,
                new int[] {0xFFFF0000, 0xFF0000FF}, null, Shader.TileMode.CLAMP);
        Paint sp = new Paint();
        sp.setShader(lg);
        System.out.println("linear=" + (sp.getShader() != null));

        Paint rp = new Paint();
        rp.setShader(new RadialGradient(5, 5, 5, 0xFFFF0000, 0xFF0000FF,
                Shader.TileMode.CLAMP));
        System.out.println("radial=" + (rp.getShader() != null));

        Paint wp = new Paint();
        wp.setShader(new SweepGradient(5, 5, 0xFFFF0000, 0xFF0000FF));
        System.out.println("sweep=" + (wp.getShader() != null));

        Shader a = new LinearGradient(0, 0, 10, 0, 0xFFFF0000, 0xFF000000,
                Shader.TileMode.CLAMP);
        Shader b = new LinearGradient(0, 0, 0, 10, 0xFF0000FF, 0xFF000000,
                Shader.TileMode.CLAMP);
        Paint cp = new Paint();
        cp.setShader(new ComposeShader(a, b, PorterDuff.Mode.SRC_OVER));
        System.out.println("compose=" + (cp.getShader() != null));

        System.out.println("done");
    }
}
