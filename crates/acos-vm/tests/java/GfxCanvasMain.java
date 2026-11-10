// Java-level acceptance for the T-31/T-32 display-list + HardwareRenderer path.
//
// Records a display list with the real `RecordingCanvas` (rect + text), renders
// it through the real `HardwareRenderer` state machine, and round-trips 30
// `RenderNode` properties. Prints a machine-checkable summary on stdout; the
// Rust test reads the rasterised frame from the FrameSink.
import android.graphics.HardwareRenderer;
import android.graphics.Paint;
import android.graphics.RecordingCanvas;
import android.graphics.RenderNode;

public class GfxCanvasMain {
    static int failures = 0;

    static void check(String name, boolean cond) {
        if (!cond) {
            failures++;
            System.out.println("FAIL " + name);
        }
    }

    static void eq(String name, float got, float want) {
        check(name + " got=" + got + " want=" + want, got == want);
    }

    static void eqi(String name, int got, int want) {
        check(name + " got=" + got + " want=" + want, got == want);
    }

    public static void main(String[] args) {
        // --- record a display list on a real RenderNode --------------------
        RenderNode content = new RenderNode("content");
        content.setPosition(0, 0, 120, 120);
        RecordingCanvas c = content.beginRecording();
        Paint rectPaint = new Paint();
        rectPaint.setColor(0xFF3366FF); // opaque blue
        c.drawRect(0, 0, 120, 60, rectPaint);
        Paint textPaint = new Paint();
        textPaint.setColor(0xFFFFFFFF);
        textPaint.setTextSize(24f);
        c.drawText("Hi", 4, 100, textPaint);
        content.endRecording();

        // Record-time capture: mutate the paint *after* recording. The recorded
        // rect must stay blue, not become red.
        rectPaint.setColor(0xFFFF0000);

        // --- drive the HardwareRenderer state machine ----------------------
        HardwareRenderer hr = new HardwareRenderer();
        hr.setContentRoot(content);
        hr.setSurface(null);
        int r1 = hr.createRenderRequest().syncAndDraw();
        int r2 = hr.createRenderRequest().syncAndDraw();
        System.out.println("sync=" + r1 + "," + r2);

        // --- 30 RenderNode property round-trips ----------------------------
        RenderNode n = new RenderNode("props");
        n.setPosition(10, 20, 130, 220);
        eqi("left", n.getLeft(), 10);
        eqi("top", n.getTop(), 20);
        eqi("right", n.getRight(), 130);
        eqi("bottom", n.getBottom(), 220);
        eqi("width", n.getWidth(), 120);
        eqi("height", n.getHeight(), 200);
        n.setAlpha(0.5f);
        eq("alpha", n.getAlpha(), 0.5f);
        n.setTranslationX(3f);
        eq("tx", n.getTranslationX(), 3f);
        n.setTranslationY(4f);
        eq("ty", n.getTranslationY(), 4f);
        n.setTranslationZ(5f);
        eq("tz", n.getTranslationZ(), 5f);
        n.setScaleX(2f);
        eq("sx", n.getScaleX(), 2f);
        n.setScaleY(3f);
        eq("sy", n.getScaleY(), 3f);
        n.setRotationZ(45f);
        eq("rotZ", n.getRotationZ(), 45f);
        n.setRotationX(10f);
        eq("rotX", n.getRotationX(), 10f);
        n.setRotationY(20f);
        eq("rotY", n.getRotationY(), 20f);
        n.setPivotX(1f);
        eq("pivotX", n.getPivotX(), 1f);
        n.setPivotY(2f);
        eq("pivotY", n.getPivotY(), 2f);
        check("pivotExplicit", n.isPivotExplicitlySet());
        n.resetPivot();
        check("pivotReset", !n.isPivotExplicitlySet());
        n.setClipToBounds(true);
        check("clipToBounds", n.getClipToBounds());
        n.setClipToOutline(true);
        check("clipToOutline", n.getClipToOutline());
        n.setElevation(9f);
        eq("elevation", n.getElevation(), 9f);
        n.setAmbientShadowColor(0x11223344);
        eqi("ambient", n.getAmbientShadowColor(), 0x11223344);
        n.setSpotShadowColor(0x55667788);
        eqi("spot", n.getSpotShadowColor(), 0x55667788);
        n.setHasOverlappingRendering(false);
        check("overlapping", !n.hasOverlappingRendering());
        n.setForceDarkAllowed(false);
        check("forceDark", !n.isForceDarkAllowed());
        n.setCameraDistance(1234f);
        eq("camera", n.getCameraDistance(), 1234f);
        n.setPosition(10, 20, 130, 220);
        n.offsetLeftAndRight(5);
        eqi("offX.l", n.getLeft(), 15);
        eqi("offX.r", n.getRight(), 135);
        n.offsetTopAndBottom(7);
        eqi("offY.t", n.getTop(), 27);
        eqi("offY.b", n.getBottom(), 227);

        System.out.println("props=" + (failures == 0 ? "ok" : "FAIL"));
        System.out.println("done");
    }
}
