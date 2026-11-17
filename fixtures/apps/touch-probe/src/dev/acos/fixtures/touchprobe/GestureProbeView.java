package dev.acos.fixtures.touchprobe;

import android.content.Context;
import android.util.AttributeSet;
import android.util.Log;
import android.view.GestureDetector;
import android.view.MotionEvent;
import android.view.View;
import android.widget.TextView;

/**
 * A non-clickable custom view running a real {@link GestureDetector} in
 * {@code onTouchEvent} — the common "gesture surface" shape (a fling/long-press
 * on a view that is not clickable and has no click listener).
 *
 * <p>AOSP feeds {@code onTouchEvent} even when no listener consumed the DOWN
 * ({@code View.dispatchTouchEvent}: listener first, then
 * {@code if (!result && onTouchEvent(event))}). This view consumes the DOWN
 * through its detector, which makes it a touch target and therefore the stream
 * it tracks keeps arriving.
 */
public class GestureProbeView extends View {
    private static final String TAG = "AcosTouchProbe";
    private final GestureDetector detector;
    private TextView statusView;

    public GestureProbeView(Context context, AttributeSet attrs) {
        super(context, attrs);
        detector = new GestureDetector(context, new GestureDetector.SimpleOnGestureListener() {
            @Override
            public boolean onDown(MotionEvent e) {
                Log.i(TAG, "gesture.onDown");
                return true;
            }

            @Override
            public void onLongPress(MotionEvent e) {
                Log.i(TAG, "gesture.onLongPress x=" + e.getX() + " y=" + e.getY());
                if (statusView != null) {
                    statusView.setText(R.string.long_pressed);
                }
            }

            @Override
            public boolean onFling(MotionEvent from, MotionEvent to, float vx, float vy) {
                Log.i(TAG, "gesture.onFling vx=" + vx + " vy=" + vy);
                if (statusView != null) {
                    statusView.setText(R.string.flung);
                }
                return true;
            }
        });
    }

    public void setStatusView(TextView status) {
        statusView = status;
    }

    @Override
    public boolean dispatchTouchEvent(MotionEvent event) {
        Log.i(TAG, "gesture.dispatchTouchEvent " + MainActivity.action(event)
                + " x=" + event.getX() + " y=" + event.getY());
        return super.dispatchTouchEvent(event);
    }

    @Override
    public boolean onTouchEvent(MotionEvent event) {
        boolean handled = detector.onTouchEvent(event);
        Log.i(TAG, "gesture.onTouchEvent " + MainActivity.action(event) + " detector=" + handled);
        if (event.getActionMasked() == MotionEvent.ACTION_DOWN) {
            // A gesture surface that tracks a stream must claim the DOWN,
            // otherwise the framework never offers it another event.
            return true;
        }
        return handled;
    }
}
