import android.os.Looper;
import android.os.Parcel;
import android.view.InputChannel;
import android.view.InputDevice;
import android.view.InputEvent;
import android.view.InputEventReceiver;
import android.view.InputEventSender;
import android.view.MotionEvent;

/**
 * T-36 InputChannel parcel acceptance: a channel written to a real
 * {@code Parcel} and read back is the same logical channel. The read-back end is
 * used as the receiver while the original sender end writes, so delivery proves
 * the descriptor (and name/token) survived the parcel.
 */
public class InputParcelMain {
    static int received = 0;

    static class Receiver extends InputEventReceiver {
        Receiver(InputChannel c, Looper l) {
            super(c, l);
        }

        @Override
        public void onInputEvent(InputEvent event) {
            if (event instanceof MotionEvent) {
                received++;
                finishInputEvent(event, true);
            }
            Looper.myLooper().quitSafely();
        }
    }

    public static void main(String[] args) {
        Looper.prepare();
        InputChannel[] pair = InputChannel.openInputChannelPair("acos-parcel-test");
        String originalName = pair[1].getName();

        Parcel parcel = Parcel.obtain();
        pair[1].writeToParcel(parcel, 0);
        parcel.setDataPosition(0);
        InputChannel readBack = new InputChannel();
        readBack.readFromParcel(parcel);
        String readName = readBack.getName();
        parcel.recycle();

        System.out.println("name=" + originalName);
        System.out.println("readName=" + readName);

        Receiver receiver = new Receiver(readBack, Looper.myLooper());
        InputEventSender sender = new InputEventSender(pair[0], Looper.myLooper());
        MotionEvent me = MotionEvent.obtain(
                1_000_000_000L, 1_000_050_000L, MotionEvent.ACTION_DOWN,
                11.5f, 22.25f, 1.0f, 1.0f, 0, 1.0f, 1.0f, 0, 0);
        me.setSource(InputDevice.SOURCE_TOUCHSCREEN);
        boolean sent = sender.sendInputEvent(7, me);
        me.recycle();
        System.out.println("sent=" + sent);

        Looper.loop();
        System.out.println("received=" + received);

        sender.dispose();
        receiver.dispose();
        readBack.dispose();
        pair[0].dispose();
        pair[1].dispose();
    }
}
