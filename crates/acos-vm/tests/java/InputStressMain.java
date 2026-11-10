import android.os.Looper;
import android.view.InputChannel;
import android.view.InputDevice;
import android.view.InputEvent;
import android.view.InputEventReceiver;
import android.view.InputEventSender;
import android.view.MotionEvent;

/**
 * T-36 leak check: 1000 real {@link MotionEvent}s travel through a real
 * {@code InputChannel} pair and are finished exactly once each. The Rust test
 * asserts that no native struct, channel, receiver or descriptor leaks.
 *
 * <p>The sender runs on a second green thread: an {@code AF_UNIX} seqpacket
 * socket has a bounded receive queue, so the app must drain while it produces.
 */
public class InputStressMain {
    static int received = 0;
    static int finished = 0;

    static class Receiver extends InputEventReceiver {
        Receiver(InputChannel c, Looper l) {
            super(c, l);
        }

        @Override
        public void onInputEvent(InputEvent event) {
            if (event instanceof MotionEvent) {
                received++;
                finishInputEvent(event, true);
                finished++;
            }
            if (received >= 1000) {
                Looper.myLooper().quitSafely();
            }
        }
    }

    public static void main(String[] args) {
        Looper.prepare();
        InputChannel[] pair = InputChannel.openInputChannelPair("acos-input-stress");
        Receiver receiver = new Receiver(pair[1], Looper.myLooper());
        final InputEventSender sender = new InputEventSender(pair[0], Looper.myLooper());

        Thread producer = new Thread(new Runnable() {
            @Override
            public void run() {
                for (int i = 0; i < 1000; i++) {
                    MotionEvent me = MotionEvent.obtain(
                            1_000_000_000L + i, 1_000_000_000L + i + 1,
                            MotionEvent.ACTION_MOVE, i, i, 1.0f, 1.0f, 0, 1.0f, 1.0f, 0, 0);
                    me.setSource(InputDevice.SOURCE_TOUCHSCREEN);
                    sender.sendInputEvent(i + 1, me);
                    me.recycle();
                    try {
                        Thread.sleep(1);
                    } catch (InterruptedException e) {
                        // ignore
                    }
                }
            }
        });
        producer.start();

        Looper.loop();

        System.out.println("received=" + received);
        System.out.println("finished=" + finished);

        sender.dispose();
        receiver.dispose();
        pair[0].dispose();
        pair[1].dispose();
    }
}
