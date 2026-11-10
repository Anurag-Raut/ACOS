import android.os.Handler;
import android.os.Looper;
import android.view.InputChannel;
import android.view.InputDevice;
import android.view.InputEvent;
import android.view.InputEventReceiver;
import android.view.InputEventSender;
import android.view.MotionEvent;

/**
 * T-36 Java-level acceptance: a real {@link MotionEvent} built by the real
 * {@code MotionEvent.obtain} is sent through a real {@code InputChannel} pair by
 * a real {@code InputEventSender} and received by a real
 * {@code InputEventReceiver} subclass on the app {@code Looper}, which calls
 * {@code finishInputEvent}. Exercises the 16 required MotionEvent natives
 * through their public API.
 */
public class InputMain {
    static final StringBuilder out = new StringBuilder();
    static int received = 0;
    static int finished = 0;
    static int downAction = -1;
    static int upAction = -1;
    static int downFlags = -1;
    static float downX = -1f;
    static float downY = -1f;
    static float upX = -1f;
    static float upY = -1f;

    static class Receiver extends InputEventReceiver {
        Receiver(InputChannel c, Looper l) {
            super(c, l);
        }

        @Override
        public void onInputEvent(InputEvent event) {
            if (event instanceof MotionEvent) {
                MotionEvent m = (MotionEvent) event;
                received++;
                int action = m.getAction();
                if (action == MotionEvent.ACTION_DOWN) {
                    downAction = action;
                    downFlags = m.getFlags();
                    downX = m.getX();
                    downY = m.getY();
                } else if (action == MotionEvent.ACTION_UP) {
                    upAction = action;
                    upX = m.getX();
                    upY = m.getY();
                }
                // Exercise the required natives reachable through the public SDK.
                int pc = m.getPointerCount();
                int pid = m.getPointerId(0);
                int tool = m.getToolType(0);
                int src = m.getSource();
                int bs = m.getButtonState();
                int flags = m.getFlags();
                long dt = m.getDownTime();
                long et = m.getEventTime();
                float ax = m.getAxisValue(MotionEvent.AXIS_X);
                float rawX = m.getRawX();
                m.offsetLocation(1f, 0f);
                m.offsetLocation(-1f, 0f);
                m.setAction(action);
                out.append("event ")
                        .append(received)
                        .append(": pc=").append(pc)
                        .append(" pid=").append(pid)
                        .append(" tool=").append(tool)
                        .append(" src=0x").append(Integer.toHexString(src))
                        .append(" bs=").append(bs)
                        .append(" flags=").append(flags)
                        .append(" dt=").append(dt)
                        .append(" et=").append(et)
                        .append(" ax=").append(ax)
                        .append(" rawX=").append(rawX)
                        .append('\n');
                finishInputEvent(event, true);
                finished++;
            }
            if (received >= 2) {
                Looper.myLooper().quitSafely();
            }
        }
    }

    public static void main(String[] args) {
        Looper.prepare();
        InputChannel[] pair = InputChannel.openInputChannelPair("acos-input-test");
        Receiver receiver = new Receiver(pair[1], Looper.myLooper());
        InputEventSender sender = new InputEventSender(pair[0], Looper.myLooper());

        long downTime = 1_000_000_000L;
        long eventTime = 1_000_050_000L;
        MotionEvent down = MotionEvent.obtain(
                downTime, eventTime, MotionEvent.ACTION_DOWN,
                540.125f, 427.875f, 1.0f, 1.0f, 0, 1.0f, 1.0f, 0, 0);
        down.setSource(InputDevice.SOURCE_TOUCHSCREEN);
        int sentFlags = down.getFlags();
        boolean s1 = sender.sendInputEvent(1, down);

        MotionEvent up = MotionEvent.obtain(
                downTime, eventTime + 50_000_000L, MotionEvent.ACTION_UP,
                540.125f, 427.875f, 1.0f, 1.0f, 0, 1.0f, 1.0f, 0, 0);
        up.setSource(InputDevice.SOURCE_TOUCHSCREEN);
        boolean s2 = sender.sendInputEvent(2, up);

        down.recycle();
        up.recycle();

        // Safety net: never let the looper hang.
        new Handler(Looper.myLooper()).postDelayed(new Runnable() {
            @Override
            public void run() {
                Looper.myLooper().quitSafely();
            }
        }, 3000);

        Looper.loop();

        out.append("sent1=").append(s1).append('\n');
        out.append("sent2=").append(s2).append('\n');
        out.append("sentFlags=").append(sentFlags).append('\n');
        out.append("received=").append(received).append('\n');
        out.append("finished=").append(finished).append('\n');
        out.append("downAction=").append(downAction).append('\n');
        out.append("upAction=").append(upAction).append('\n');
        out.append("downFlags=").append(downFlags).append('\n');
        out.append("downX=").append(downX).append('\n');
        out.append("downY=").append(downY).append('\n');
        out.append("upX=").append(upX).append('\n');
        out.append("upY=").append(upY).append('\n');
        System.out.print(out);

        sender.dispose();
        receiver.dispose();
        pair[0].dispose();
        pair[1].dispose();
    }
}
