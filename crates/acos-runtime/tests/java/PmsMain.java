import android.content.ComponentName;
import android.content.pm.ActivityInfo;
import android.content.pm.ApplicationInfo;
import android.content.pm.IPackageManager;
import android.content.pm.PackageInfo;
import android.os.IBinder;
import android.os.ServiceManager;

/**
 * T-22 acceptance: the real {@code IPackageManager$Stub$Proxy} reads the
 * {@code ApplicationInfo} / {@code PackageInfo} / {@code ActivityInfo} the
 * ACOS PMS materialises through the Java bridge.
 *
 * <p>Prints one {@code key=value} line per field so the Rust golden test can
 * diff the output against a committed expectation derived from the reference
 * captures. Paths are reduced to their base name (the absolute APK path is
 * environment specific).
 */
public final class PmsMain {
    static final String PKG = "dev.acos.fixtures.helloviews";
    static final String ACTIVITY = PKG + ".MainActivity";

    static void check(boolean cond, String msg) {
        if (!cond) {
            throw new AssertionError(msg);
        }
    }

    static void out(String key, Object value) {
        System.out.println(key + "=" + value);
    }

    static String base(String path) {
        if (path == null) {
            return "<null>";
        }
        int slash = path.lastIndexOf('/');
        return slash >= 0 ? path.substring(slash + 1) : path;
    }

    static String tail2(String path) {
        if (path == null) {
            return "<null>";
        }
        int end = path.lastIndexOf('/');
        if (end < 0) {
            return path;
        }
        int start = path.lastIndexOf('/', end - 1);
        return start >= 0 ? path.substring(start + 1) : path.substring(end + 1);
    }

    public static void main(String[] args) throws Exception {
        IBinder binder = ServiceManager.getService("package");
        check(binder != null, "getService(package) must return a binder");
        IPackageManager pm = IPackageManager.Stub.asInterface(binder);
        check(pm != null, "asInterface(package) must produce a proxy");

        ApplicationInfo app = pm.getApplicationInfo(PKG, 0L, 0);
        check(app != null, "getApplicationInfo must not be null");
        out("app.packageName", app.packageName);
        out("app.name", app.name == null ? "<null>" : app.name);
        out("app.className", app.className == null ? "<null>" : app.className);
        out("app.sourceDir", base(app.sourceDir));
        out("app.publicSourceDir", base(app.publicSourceDir));
        out("app.dataDir", app.dataDir);
        out("app.nativeLibraryDir", tail2(app.nativeLibraryDir));
        out("app.uid", app.uid);
        out("app.flags", app.flags);
        out("app.targetSdkVersion", app.targetSdkVersion);
        out("app.minSdkVersion", app.minSdkVersion);
        out("app.theme", app.theme);
        out("app.labelRes", app.labelRes);

        PackageInfo pkg = pm.getPackageInfo(PKG, 0L, 0);
        check(pkg != null, "getPackageInfo must not be null");
        out("pkg.packageName", pkg.packageName);
        out("pkg.versionCode", pkg.versionCode);
        out("pkg.versionName", pkg.versionName);
        check(pkg.applicationInfo != null, "PackageInfo.applicationInfo");
        out("pkg.applicationInfo.packageName", pkg.applicationInfo.packageName);
        out("pkg.applicationInfo.sourceDir", base(pkg.applicationInfo.sourceDir));
        out("pkg.applicationInfo.uid", pkg.applicationInfo.uid);
        check(pkg.activities != null, "PackageInfo.activities");
        out("pkg.activities.length", pkg.activities.length);
        out("pkg.activities[0].name", pkg.activities[0].name);
        out("pkg.activities[0].packageName", pkg.activities[0].packageName);

        ComponentName cn = new ComponentName(PKG, ACTIVITY);
        ActivityInfo act = pm.getActivityInfo(cn, 0L, 0);
        check(act != null, "getActivityInfo must not be null");
        out("act.name", act.name);
        out("act.packageName", act.packageName);
        out("act.exported", act.exported);
        out("act.launchMode", act.launchMode);
        out("act.theme", act.theme);
        out("act.applicationInfo.packageName", act.applicationInfo.packageName);
        out("act.applicationInfo.sourceDir", base(act.applicationInfo.sourceDir));

        out("feature.touchscreen", pm.hasSystemFeature("android.hardware.touchscreen", 0));
        out("feature.webview", pm.hasSystemFeature("android.software.webview", 0));
        out("feature.unknown", pm.hasSystemFeature("android.hardware.definitely.absent", 0));

        String[] packages = pm.getPackagesForUid(10192);
        check(packages != null && packages.length == 1, "one package for uid 10192");
        out("uid.packages", packages[0]);

        out("perm.internet", pm.checkPermission("android.permission.INTERNET", PKG, 0));
        out("perm.unknown", pm.checkPermission("android.permission.CAMERA", PKG, 0));

        // Oneway: exercises the decoded `notifyDexLoad` layout.
        java.util.HashMap<String, String> dexMap = new java.util.HashMap<>();
        dexMap.put("classes.dex", "PCL[]");
        pm.notifyDexLoad(PKG, dexMap, "x86_64");

        System.out.println("pms=ok");
    }
}
