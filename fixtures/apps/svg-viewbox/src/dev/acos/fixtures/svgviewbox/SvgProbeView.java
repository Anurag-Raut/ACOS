package dev.acos.fixtures.svgviewbox;

import android.content.Context;
import android.graphics.Canvas;
import android.graphics.Color;
import android.graphics.Matrix;
import android.graphics.Paint;
import android.graphics.Path;
import android.graphics.RectF;
import android.util.AttributeSet;
import android.util.DisplayMetrics;
import android.util.Log;
import android.view.View;
import android.widget.TextView;

/**
 * A view that draws exactly like react-native-svg's Android renderer, with its numbers on screen.
 *
 * <p>react-native-svg computes its content transform from four inputs:
 *
 * <ul>
 *   <li>{@code SvgView.mScale = DisplayMetricsHolder.getScreenDisplayMetrics().density} — captured
 *       once, in the SvgView constructor ({@code SvgView.java}, {@code VirtualView.java});
 *   <li>{@code SvgView.getViewBox()} — the {@code viewBox} attribute multiplied by that density
 *       ({@code minX*mScale, minY*mScale, (minX+w)*mScale, (minY+h)*mScale});
 *   <li>{@code drawChildren} — the element rect is {@code canvas.getWidth()/getHeight()};
 *   <li>{@code PathParser} — path data coordinates multiplied by density when the Android
 *       {@link Path} is built.
 * </ul>
 *
 * <p>Both density multiplications cancel only when the canvas is measured in device pixels: the
 * ink then lands inside the view box at {@code capDp*density} pixels. This probe draws a 1800x439
 * unit "ink" rectangle (the wordmark's own ink extents) in a 1833x650 viewBox carried by a
 * 270x116 px view — the exact geometry Gaten's welcome wordmark has at 1080x2400 @ 2.625 — and
 * prints the canvas size, density, transform and the ink rect it expects, so a host can compare a
 * device's values with a guest's.
 */
public class SvgProbeView extends View {
    public static final String TAG = "AcosSvgProbe";

    // The wordmark's own numbers (src/rn/ui/tabHeadingGlyphs.ts + GatenWordmark.tsx).
    private static final float VB_MIN_X = 0f;
    private static final float VB_MIN_Y = -30f;
    private static final float VB_W = 1833f;
    private static final float VB_H = 650f;
    private static final float INK_L = 16f;
    private static final float INK_T = 16f;
    private static final float INK_R = 1816f;
    private static final float INK_B = 455f;

    private final Paint inkPaint = new Paint(Paint.ANTI_ALIAS_FLAG);
    private final Paint refPaint = new Paint(Paint.ANTI_ALIAS_FLAG);
    private final RectF ink = new RectF();
    private float density;
    private float scale;
    private float tx;
    private float ty;
    private int canvasW;
    private int canvasH;

    public SvgProbeView(Context context, AttributeSet attrs) {
        super(context, attrs);
        DisplayMetrics metrics = getResources().getDisplayMetrics();
        density = metrics.density;
        inkPaint.setStyle(Paint.Style.STROKE);
        inkPaint.setStrokeWidth(2f);
        inkPaint.setColor(Color.BLACK);
        refPaint.setStyle(Paint.Style.FILL);
        refPaint.setColor(Color.RED);
        setWillNotDraw(false);
    }

    @Override
    protected void onDraw(Canvas canvas) {
        canvasW = canvas.getWidth();
        canvasH = canvas.getHeight();
        // react-native-svg SvgView.getViewBox(): the viewBox multiplied by density.
        RectF vb = new RectF(
                VB_MIN_X * density, VB_MIN_Y * density,
                (VB_MIN_X + VB_W) * density, (VB_MIN_Y + VB_H) * density);
        // SvgView.drawChildren(): the element rect is the canvas size.
        RectF e = new RectF(0f, 0f, canvasW, canvasH);
        // ViewBox.getTransform(..., "xMidYMid", meet): JSON's align check is "YMid", but either
        // branch is the same arithmetic for a mid/mid meet.
        float sx = e.width() / vb.width();
        float sy = e.height() / vb.height();
        scale = Math.min(sx, sy);
        tx = -vb.left * scale + (e.width() - vb.width() * scale) / 2f;
        ty = -vb.top * scale + (e.height() - vb.height() * scale) / 2f;

        // PathParser: path data * density, then the viewBox matrix.
        Path path = new Path();
        path.moveTo(INK_L * density, INK_T * density);
        path.lineTo(INK_R * density, INK_T * density);
        path.lineTo(INK_R * density, INK_B * density);
        path.lineTo(INK_L * density, INK_B * density);
        path.close();

        Matrix m = new Matrix();
        m.postTranslate(tx, ty);
        m.preScale(scale, scale);
        path.transform(m);
        path.computeBounds(ink, false);

        StringBuilder sb = new StringBuilder();
        sb.append("view=").append(getWidth()).append('x').append(getHeight());
        sb.append(" canvas=").append(canvasW).append('x').append(canvasH);
        sb.append(" density=").append(density);
        sb.append(" canvasIsViewPx=").append(canvasW == getWidth() && canvasH == getHeight());
        sb.append(" scale=").append(scale);
        sb.append(" ink=").append(Math.round(ink.left)).append(',').append(Math.round(ink.top))
                .append(',').append(Math.round(ink.right)).append(',').append(Math.round(ink.bottom));
        sb.append(" inkInsideView=").append(ink.left >= -1 && ink.top >= -1
                && ink.right <= getWidth() + 1 && ink.bottom <= getHeight() + 1);
        Matrix cm = canvas.getMatrix();
        float[] v = new float[9];
        cm.getValues(v);
        sb.append(" canvasMatrix=").append(v[0]).append(',').append(v[2]).append(',')
                .append(v[4]).append(',').append(v[5]);
        sb.append(" sdk=").append(android.os.Build.VERSION.SDK_INT);
        Log.i(TAG, sb.toString());
        View root = getRootView();
        TextView out = root == null ? null : root.findViewById(R.id.svg_metrics);
        if (out != null) {
            out.setText(sb.toString());
        }

        // The react-native-svg path: density-scaled geometry through the viewBox matrix.
        int save = canvas.save();
        canvas.concat(m);
        canvas.scale(density, density);
        canvas.drawPath(pathFromUnits(), inkPaint);
        canvas.restoreToCount(save);

        // Device-pixel reference marks: a 20 px square in each corner of the canvas. If these do
        // not land exactly on the view's corners, the canvas is not in device pixels.
        canvas.drawRect(0f, 0f, 20f, 20f, refPaint);
        canvas.drawRect(canvasW - 20f, 0f, canvasW, 20f, refPaint);
        canvas.drawRect(0f, canvasH - 20f, 20f, canvasH, refPaint);
        canvas.drawRect(canvasW - 20f, canvasH - 20f, canvasW, canvasH, refPaint);
    }

    private Path pathFromUnits() {
        Path path = new Path();
        path.moveTo(INK_L, INK_T);
        path.lineTo(INK_R, INK_T);
        path.lineTo(INK_R, INK_B);
        path.lineTo(INK_L, INK_B);
        path.close();
        return path;
    }
}
