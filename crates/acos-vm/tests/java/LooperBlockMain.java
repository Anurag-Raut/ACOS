import android.os.Looper;

/**
 * A looper with no messages. Blocks in {@code nativePollOnce(-1)} until the
 * process is killed; the integration test asserts the process burns no CPU
 * while it is blocked.
 */
public class LooperBlockMain {
    public static void main(String[] args) {
        Looper.prepare();
        Looper.loop();
    }
}
