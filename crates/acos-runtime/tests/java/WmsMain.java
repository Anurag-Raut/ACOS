import android.os.IBinder;
import android.os.ServiceManager;
import android.view.IWindowManager;
import android.view.IWindowSession;

/**
 * T-29 acceptance at the Java level: the real {@code IWindowManager$Stub$Proxy}
 * reads the ACOS WMS replies over the real Binder transport.
 *
 * <p>Runs on the real Android 14 core library + framework DEX. The ACOS WMS is
 * the in-process Rust state machine ({@code acos-wm} + {@code acos-services::
 * wms}); the parcel bytes are written by {@code acos-runtime::wms_java} using
 * the DEX-derived codecs.
 *
 * <p>{@code addToDisplayAsUser}/{@code relayout} are covered at the parcel level
 * by {@code tests/wms.rs}: their reply carries an {@code android.view.InputChannel}
 * whose {@code nativeReadFromParcel} is still an unimplemented native (T-36,
 * input agent), so the Java proxy cannot yet materialise it.
 */
public final class WmsMain {
    static void check(boolean cond, String msg) {
        if (!cond) {
            throw new AssertionError(msg);
        }
    }

    static void out(String key, Object value) {
        System.out.println(key + "=" + value);
    }

    public static void main(String[] args) throws Exception {
        IBinder binder = ServiceManager.getService("window");
        check(binder != null, "getService(window) must return a binder");
        IWindowManager wm = IWindowManager.Stub.asInterface(binder);
        check(wm != null, "asInterface(window) must produce a proxy");

        out("useBLAST", wm.useBLAST());
        out("getCurrentAnimatorScale", wm.getCurrentAnimatorScale());
        out("getBaseDisplayDensity", wm.getBaseDisplayDensity(0));
        out("hasNavigationBar", wm.hasNavigationBar(0));
        out("isInTouchMode", wm.isInTouchMode(0));
        wm.setInTouchMode(true, 0);
        out("setInTouchMode", "ok");

        IWindowSession session = wm.openSession(null);
        check(session != null, "openSession must return an IWindowSession binder");
        out("openSession", "ok");

        System.out.println("wms=ok");
    }
}
