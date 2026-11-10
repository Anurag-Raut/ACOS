package dev.acos.fixtures.canvasshapes;

import android.content.Context;
import android.graphics.Canvas;
import android.graphics.Color;
import android.graphics.Paint;
import android.graphics.Path;
import android.graphics.RectF;
import android.util.AttributeSet;
import android.util.Log;
import android.view.MotionEvent;
import android.view.View;

import java.util.ArrayList;
import java.util.List;

/**
 * Custom {@link View} that draws with {@link Canvas}/{@link Paint}/{@link Path}
 * and reacts to touch. Plain framework APIs only.
 *
 * Observable behaviour:
 *  - {@link #onDraw} fills a background, draws a rect, a circle, a stroked
 *    line, a quadratic {@link Path}, and {@link Paint#drawText}
 *  - every touch down records the point and calls {@link #invalidate()}
 *  - the colour of the circle depends on the tap count, so a tap changes the
 *    frame deterministically
 *  - logs with tag "AcosCanvas"
 */
public class ShapeView extends View {
    private static final String TAG = "AcosCanvas";

    private final Paint fill = new Paint(Paint.ANTI_ALIAS_FLAG);
    private final Paint stroke = new Paint(Paint.ANTI_ALIAS_FLAG);
    private final Paint text = new Paint(Paint.ANTI_ALIAS_FLAG);
    private final Path path = new Path();
    private final RectF oval = new RectF();
    private final List<float[]> taps = new ArrayList<float[]>();

    private int drawCount = 0;

    public ShapeView(Context context) {
        super(context);
        init();
    }

    public ShapeView(Context context, AttributeSet attrs) {
        super(context, attrs);
        init();
    }

    public ShapeView(Context context, AttributeSet attrs, int defStyleAttr) {
        super(context, attrs, defStyleAttr);
        init();
    }

    private void init() {
        stroke.setStyle(Paint.Style.STROKE);
        stroke.setStrokeWidth(6f);
        stroke.setColor(Color.rgb(0x22, 0x66, 0xCC));
        text.setColor(Color.rgb(0x11, 0x11, 0x11));
        text.setTextSize(48f);
        setFocusable(true);
        Log.i(TAG, "init");
    }

    @Override
    protected void onDraw(Canvas canvas) {
        super.onDraw(canvas);
        drawCount++;
        int w = getWidth();
        int h = getHeight();

        canvas.drawColor(Color.rgb(0xF5, 0xF5, 0xEA));

        fill.setColor(Color.rgb(0xE0, 0x60, 0x40));
        canvas.drawRect(40f, 40f, w - 40f, 180f, fill);

        int color = (taps.size() % 2 == 0) ? Color.rgb(0x30, 0x80, 0x40)
                                           : Color.rgb(0x80, 0x30, 0x80);
        fill.setColor(color);
        float cx = w / 2f;
        float cy = h / 2f;
        float r = Math.min(w, h) / 5f;
        canvas.drawCircle(cx, cy, r, fill);

        canvas.drawLine(40f, h - 40f, w - 40f, h - 40f, stroke);

        path.reset();
        path.moveTo(40f, h * 0.75f);
        path.quadTo(cx, h * 0.55f, w - 40f, h * 0.75f);
        canvas.drawPath(path, stroke);

        canvas.drawText("shapes drawn=" + drawCount, 40f, h * 0.9f, text);
        for (float[] p : taps) {
            fill.setColor(Color.rgb(0x10, 0x10, 0x10));
            canvas.drawCircle(p[0], p[1], 10f, fill);
        }
        oval.set(cx - r, cy - r, cx + r, cy + r);
        canvas.drawArc(oval, 200f, 140f, false, stroke);
        Log.i(TAG, "onDraw n=" + drawCount + " taps=" + taps.size() + " w=" + w + " h=" + h);
    }

    @Override
    public boolean onTouchEvent(MotionEvent event) {
        int action = event.getActionMasked();
        if (action == MotionEvent.ACTION_DOWN) {
            taps.add(new float[] {event.getX(), event.getY()});
            Log.i(TAG, "touch down x=" + event.getX() + " y=" + event.getY()
                    + " taps=" + taps.size());
            invalidate();
            return true;
        }
        if (action == MotionEvent.ACTION_UP) {
            performClick();
            return true;
        }
        return super.onTouchEvent(event);
    }

    @Override
    public boolean performClick() {
        super.performClick();
        Log.i(TAG, "performClick");
        return true;
    }
}
