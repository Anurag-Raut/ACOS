import android.app.IActivityClientController;
import android.os.Binder;
import android.os.IBinder;
import android.os.ParcelFileDescriptor;
import android.os.ServiceManager;
import android.view.IGraphicsStats;
import android.view.IGraphicsStatsCallback;

/**
 * T-19 acceptance: the single outgoing native
 * {@code BinderProxy.transactNative} drives a fake in-process service through a
 * real framework {@code $Stub$Proxy}, and a {@code system -> app} delivery
 * re-enters the caller's own stub via {@code Binder.execTransact}.
 *
 * <p>Covered: a string argument, a strong-binder argument, a
 * {@code ParcelFileDescriptor} reply, a oneway call whose handler throws (the
 * error is swallowed), and the thread the re-entrant callback lands on.
 */
public final class BinderMain {
    static volatile long callbackThread = -1;
    static volatile int callbackCount = 0;

    static void check(boolean cond, String msg) {
        if (!cond) {
            throw new AssertionError(msg);
        }
    }

    public static void main(String[] args) throws Exception {
        long mainThread = Thread.currentThread().getId();

        IBinder binder = ServiceManager.getService("test.svc");
        check(binder != null, "getService(test.svc) must return a binder");
        IGraphicsStats stats = IGraphicsStats.Stub.asInterface(binder);
        check(stats != null, "asInterface must produce a proxy");

        IGraphicsStatsCallback callback = new IGraphicsStatsCallback.Stub() {
            @Override
            public void onRotateGraphicsStatsBuffer() {
                callbackThread = Thread.currentThread().getId();
                callbackCount++;
            }
        };

        ParcelFileDescriptor pfd =
                stats.requestBufferForProcess("dev.acos.fixtures.helloviews", callback);
        check(pfd != null, "requestBufferForProcess must return a ParcelFileDescriptor");
        int fd = pfd.getFd();
        check(fd >= 0, "returned fd must be valid, got " + fd);

        // The Rust handler queued a delivery back into the callback stub; it
        // must have run through the real Binder.execTransact on this looper.
        check(callbackCount == 1, "callback must run exactly once, got " + callbackCount);
        check(callbackThread == mainThread,
                "callback must land on the looper thread: callback=" + callbackThread
                        + " main=" + mainThread);

        // A oneway transaction whose handler fails must not throw to the caller.
        IBinder oneway = ServiceManager.getService("test.oneway");
        check(oneway != null, "getService(test.oneway)");
        IActivityClientController controller =
                IActivityClientController.Stub.asInterface(oneway);
        IBinder token = new Binder();
        controller.activityResumed(token, true); // oneway: error is swallowed
        System.out.println("binder=ok fd=" + fd + " cbThread=" + callbackThread
                + " main=" + mainThread);
    }
}
