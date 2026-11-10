import android.content.pm.IPackageManager;
import android.os.IBinder;
import android.os.ServiceManager;

/**
 * T-21 acceptance: {@code ServiceManager.getService(name)} returns a working
 * proxy for a registered service and {@code null} for an unknown name.
 *
 * <p>Runs on the real Android 14 core library + framework DEX. The services are
 * the in-process Rust {@code ServiceHost} (mini-PMS), reached through the real
 * {@code IServiceManager$Stub$Proxy} / {@code IPackageManager$Stub$Proxy}
 * bytecode and the single ACOS native {@code BinderProxy.transactNative}.
 */
public final class RegistryMain {
    static void check(boolean cond, String msg) {
        if (!cond) {
            throw new AssertionError(msg);
        }
    }

    public static void main(String[] args) throws Exception {
        // A known service resolves to a binder that asInterface turns into a
        // real IPackageManager proxy.
        IBinder binder = ServiceManager.getService("package");
        check(binder != null, "getService(package) must return a binder");
        IPackageManager pm = IPackageManager.Stub.asInterface(binder);
        check(pm != null, "asInterface(package) must produce a proxy");

        String[] packages = pm.getPackagesForUid(10192);
        check(packages != null && packages.length == 1,
                "exactly one package for uid 10192, got "
                        + (packages == null ? "null" : Integer.toString(packages.length)));
        check("dev.acos.fixtures.helloviews".equals(packages[0]),
                "package name, got " + packages[0]);

        // checkService shares the table.
        IBinder checked = ServiceManager.checkService("package");
        check(checked != null, "checkService(package) must return a binder");

        // Unknown names are null, the framework then takes its real failure path.
        IBinder missing = ServiceManager.getService("definitely.not.a.service");
        check(missing == null, "unknown service must be null");
        IBinder missing2 = ServiceManager.checkService("also.missing");
        check(missing2 == null, "unknown checkService must be null");

        System.out.println("registry=ok " + packages[0]);
    }
}
