import android.os.Looper;

/**
 * A parked looper with one extra (finished) green thread.
 *
 * This is the shape that exposed the vm-perf scheduler regression: the looper
 * thread parks in {@code nativePollOnce(-1)}, but because {@code Parked} was
 * missing from the batch yield condition and the scheduler reset every thread to
 * {@code Runnable} on its periodic pre-emption, the parked thread was re-run
 * forever at 100% host CPU. With one thread ({@code LooperBlockMain}) the bug
 * did not show.
 */
public class LooperBlockMT {
    public static void main(String[] args) throws Exception {
        Looper.prepare();
        Thread t = new Thread(new Runnable() {
            public void run() {
                try {
                    Thread.sleep(10);
                } catch (InterruptedException e) {
                    // ignore
                }
            }
        });
        t.start();
        Looper.loop();
    }
}
