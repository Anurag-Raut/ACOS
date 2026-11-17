package dev.acos.fixtures.looperwake;

import android.content.Context;
import android.graphics.Canvas;
import android.graphics.Color;
import android.graphics.Paint;
import android.os.SystemClock;
import android.util.AttributeSet;
import android.util.Log;
import android.view.View;

/**
 * A view that really draws: every {@link #onDraw} increments a counter, paints a
 * differently-sized bar and logs {@code pulse.draw n=<count> [since_invalidate_ms=N]}.
 *
 * <p>It exists so a {@code postInvalidate()} issued from a background thread is
 * observable as a <em>frame</em> (the bar moves) even when no text changes, and
 * so the delay between the background thread's request and the drawn frame is
 * the guest's own measurement (no host observation can wake the looper for it).
 */
public final class PulseView extends View {
    public static final String TAG = "AcosLooperWake";

    /**
     * Uptime at which a background thread asked for this view's invalidation,
     * or 0. The next draw reports how long it took.
     */
    static volatile long sBgInvalidateAtMs;

    private final Paint paint = new Paint(Paint.ANTI_ALIAS_FLAG);
    private int draws;

    public PulseView(Context context, AttributeSet attrs) {
        super(context, attrs);
        paint.setColor(Color.rgb(0x33, 0x99, 0xcc));
    }

    public int draws() {
        return draws;
    }

    @Override
    protected void onDraw(Canvas canvas) {
        super.onDraw(canvas);
        draws++;
        int width = Math.min(getWidth(), 120 + (draws * 40) % Math.max(1, getWidth() - 120));
        canvas.drawRect(0, 0, width, getHeight(), paint);
        long sinceInvalidate = -1;
        long at = sBgInvalidateAtMs;
        if (at != 0) {
            sinceInvalidate = SystemClock.uptimeMillis() - at;
            sBgInvalidateAtMs = 0;
        }
        Log.i(TAG, "pulse.draw n=" + draws + " since_invalidate_ms=" + sinceInvalidate
                + " t=" + SystemClock.uptimeMillis());
    }
}
