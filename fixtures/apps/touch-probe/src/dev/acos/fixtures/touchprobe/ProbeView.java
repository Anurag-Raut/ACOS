package dev.acos.fixtures.touchprobe;

import android.content.Context;
import android.graphics.Canvas;
import android.graphics.Color;
import android.graphics.Paint;
import android.util.AttributeSet;
import android.util.Log;
import android.view.MotionEvent;
import android.view.View;

/**
 * A plain, <b>non-clickable</b> custom {@link View} that logs how far a touch
 * gets: its own {@code dispatchTouchEvent} (the point the framework reaches once
 * the decor has hit-tested to it) and its {@code onTouchEvent}.
 *
 * <p>AOSP's order for an ENABLED view is dispatchTouchEvent → the
 * {@code OnTouchListener} → {@code onTouchEvent} → false. A log that shows none
 * of these lines means the event never reached this view.
 */
public class ProbeView extends View {
    private static final String TAG = "AcosTouchProbe";
    private final Paint paint = new Paint();

    public ProbeView(Context context, AttributeSet attrs) {
        super(context, attrs);
        paint.setColor(Color.rgb(0x33, 0x66, 0x99));
        paint.setStyle(Paint.Style.FILL);
    }

    @Override
    protected void onDraw(Canvas canvas) {
        super.onDraw(canvas);
        canvas.drawRect(0, 0, getWidth() / 3f, getHeight(), paint);
    }

    @Override
    public boolean dispatchTouchEvent(MotionEvent event) {
        Log.i(TAG, "probe.dispatchTouchEvent " + MainActivity.action(event)
                + " x=" + event.getX() + " y=" + event.getY()
                + " raw=" + event.getRawX() + "," + event.getRawY()
                + " src=0x" + Integer.toHexString(event.getSource())
                + " flags=0x" + Integer.toHexString(event.getFlags())
                + " enabled=" + isEnabled() + " clickable=" + isClickable());
        return super.dispatchTouchEvent(event);
    }

    @Override
    public boolean onTouchEvent(MotionEvent event) {
        Log.i(TAG, "probe.onTouchEvent " + MainActivity.action(event)
                + " x=" + event.getX() + " y=" + event.getY());
        return super.onTouchEvent(event);
    }

    @Override
    public boolean dispatchGenericMotionEvent(MotionEvent event) {
        // A touch that takes the generic-motion path instead of
        // dispatchTouchEvent is a routing bug, so name it.
        Log.i(TAG, "probe.dispatchGenericMotionEvent " + MainActivity.action(event));
        return super.dispatchGenericMotionEvent(event);
    }
}
