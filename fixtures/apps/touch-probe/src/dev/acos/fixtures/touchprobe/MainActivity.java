package dev.acos.fixtures.touchprobe;

import android.app.Activity;
import android.os.Bundle;
import android.util.Log;
import android.view.MotionEvent;
import android.view.View;
import android.widget.Button;
import android.widget.TextView;

/**
 * TOUCHDISP fixture: "unhandled ack, and the screen changes a moment later".
 *
 * <p>Nothing here is clickable except the {@code inc} Button, so every touch on
 * {@code label}, {@code probe} or {@code gesture} is a press the window answers
 * {@code finishInputEvent(handled) = false}. The framework still has to
 * <em>dispatch</em> it: AOSP {@code ViewGroup.dispatchTouchEvent} hit-tests the
 * children on the DOWN and {@code View.dispatchTouchEvent} calls
 * {@code mOnTouchListener.onTouch} for any ENABLED view, clickable or not. (An
 * unhandled DOWN never becomes a touch target, so the UP is not offered to that
 * view again — the listener has to act on the DOWN, which is what this fixture
 * does and why a listener that posted on UP looked "never called".)
 *
 * <ul>
 *   <li>{@code probe} ({@link ProbeView}): the listener returns {@code false}
 *       and <b>posts</b> a {@code setText} 100 ms out, so the state change is
 *       drawn <em>after</em> the gesture was acknowledged. This is the REL3H2
 *       finding-1 shape: the settle rule must not bind the pre-state.</li>
 *   <li>{@code gesture} ({@link GestureProbeView}): a real {@code GestureDetector}
 *       on a non-clickable view; a fling / long press writes {@code status}.</li>
 *   <li>{@code root} carries an {@code OnTouchListener}, so the label tap proves
 *       the ancestor fallback of {@code ViewGroup.dispatchTouchEvent}.</li>
 * </ul>
 */
public class MainActivity extends Activity {
    private static final String TAG = "AcosTouchProbe";

    /** How long after the DOWN the listener's posted side effect runs. */
    static final int POSTED_DELAY_MS = 100;

    private TextView countView;
    private int count = 0;

    @Override
    protected void onCreate(Bundle savedInstanceState) {
        super.onCreate(savedInstanceState);
        Log.i(TAG, "onCreate");
        setContentView(R.layout.main);
        final TextView statusView = (TextView) findViewById(R.id.status);
        countView = (TextView) findViewById(R.id.count);

        View root = findViewById(R.id.root);
        root.setOnTouchListener(new View.OnTouchListener() {
            @Override
            public boolean onTouch(View v, MotionEvent event) {
                Log.i(TAG, "root.onTouch " + action(event) + " x=" + event.getX()
                        + " y=" + event.getY());
                return false;
            }
        });

        ProbeView probe = (ProbeView) findViewById(R.id.probe);
        probe.setOnTouchListener(new View.OnTouchListener() {
            @Override
            public boolean onTouch(final View v, MotionEvent event) {
                Log.i(TAG, "probe.onTouch " + action(event) + " x=" + event.getX()
                        + " y=" + event.getY() + " t=" + event.getEventTime());
                if (event.getActionMasked() == MotionEvent.ACTION_DOWN) {
                    // The side effect is *posted*: it runs on a later looper turn,
                    // after this gesture has already been acknowledged. A listener
                    // that returns false is not a touch target, so only the DOWN
                    // ever arrives - posting here is the only shape an app can
                    // write for "a tap that defers its work".
                    v.postDelayed(new Runnable() {
                        @Override
                        public void run() {
                            Log.i(TAG, "probe.posted setText(touched-delayed)");
                            statusView.setText(R.string.touched_delayed);
                        }
                    }, POSTED_DELAY_MS);
                }
                return false;
            }
        });

        GestureProbeView gesture = (GestureProbeView) findViewById(R.id.gesture);
        gesture.setStatusView(statusView);

        ((Button) findViewById(R.id.inc)).setOnClickListener(new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                count++;
                countView.setText(String.valueOf(count));
                Log.i(TAG, "increment value=" + count);
            }
        });
    }

    /**
     * Every event that gets past {@code DecorView.dispatchTouchEvent} lands here
     * ({@code DecorView} calls {@code Window.Callback.dispatchTouchEvent}, i.e.
     * this method), which is the dividing line between "the framework dropped
     * it" and "the view tree declined it".
     */
    @Override
    public boolean dispatchTouchEvent(MotionEvent event) {
        Log.i(TAG, "activity.dispatchTouchEvent " + action(event)
                + " x=" + event.getX() + " y=" + event.getY()
                + " src=0x" + Integer.toHexString(event.getSource()));
        return super.dispatchTouchEvent(event);
    }

    static String action(MotionEvent event) {
        switch (event.getActionMasked()) {
            case MotionEvent.ACTION_DOWN:
                return "DOWN";
            case MotionEvent.ACTION_UP:
                return "UP";
            case MotionEvent.ACTION_MOVE:
                return "MOVE";
            case MotionEvent.ACTION_CANCEL:
                return "CANCEL";
            default:
                return "ACTION_" + event.getActionMasked();
        }
    }
}
