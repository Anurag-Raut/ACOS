import android.os.Handler;
import android.os.Looper;
import android.os.SystemClock;

/**
 * Exercises the deterministic virtual-time looper pump.
 *
 * A message is posted 60 seconds in the future. On the real-time scheduler the
 * process would sleep for a minute; with {@code Vm::run_virtual} the guest
 * clock is advanced to the deadline, so it fires in milliseconds of wall time
 * while the guest still observes the full 60-second delay.
 */
public class LooperVirtual {
    public static void main(String[] args) {
        Looper.prepare();
        final Handler h = new Handler(Looper.myLooper());
        final long t0 = SystemClock.uptimeMillis();
        h.postDelayed(new Runnable() {
            public void run() {
                long now = SystemClock.uptimeMillis();
                System.out.println("virtual_delayed_ms=" + (now - t0));
                Looper.myLooper().quitSafely();
            }
        }, 60000);
        Looper.loop();
    }
}
