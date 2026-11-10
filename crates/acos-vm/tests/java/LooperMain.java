import android.os.Handler;
import android.os.Looper;
import android.os.SystemClock;

/**
 * Drives the real boot-classpath {@code android.os.Looper}/{@code Handler} on
 * the ACOS VM. Exercises:
 * <ul>
 *   <li>a delayed handler message (deadline precision),</li>
 *   <li>a message posted from a second green thread ({@code nativeWake}).</li>
 * </ul>
 */
public class LooperMain {
    static final StringBuilder out = new StringBuilder();
    static int done = 0;
    static long t0;

    public static void main(String[] args) {
        Looper.prepare();
        final Handler h = new Handler(Looper.myLooper());

        t0 = SystemClock.uptimeMillis();
        h.postDelayed(new Runnable() {
            public void run() {
                long now = SystemClock.uptimeMillis();
                out.append("delayed_ms=").append(now - t0).append('\n');
                finish();
            }
        }, 100);

        Thread t = new Thread(new Runnable() {
            public void run() {
                try {
                    Thread.sleep(30);
                } catch (InterruptedException e) {
                    // ignore
                }
                h.post(new Runnable() {
                    public void run() {
                        out.append("woke_from_green_thread\n");
                        finish();
                    }
                });
            }
        });
        t.start();

        Looper.loop();
        System.out.print(out);
    }

    static void finish() {
        done++;
        if (done >= 2) {
            Looper.myLooper().quitSafely();
        }
    }
}
