package android.content.pm;

import android.content.ComponentName;
import android.content.Intent;
import android.os.IBinder;
import android.os.RemoteException;

/**
 * Signature-only stub for the {@code @hide}
 * {@code android.content.pm.IPackageManager}. The real interface + `$Stub`
 * come from the boot-classpath framework DEX at runtime.
 */
public interface IPackageManager {
    ApplicationInfo getApplicationInfo(String packageName, long flags, int userId)
            throws RemoteException;

    PackageInfo getPackageInfo(String packageName, long flags, int userId)
            throws RemoteException;

    ActivityInfo getActivityInfo(ComponentName className, long flags, int userId)
            throws RemoteException;

    ServiceInfo getServiceInfo(ComponentName className, long flags, int userId)
            throws RemoteException;

    ProviderInfo getProviderInfo(ComponentName className, long flags, int userId)
            throws RemoteException;

    InstrumentationInfo getInstrumentationInfoAsUser(ComponentName className, int flags, int userId)
            throws RemoteException;

    ResolveInfo resolveIntent(Intent intent, String resolvedType, long flags, int userId)
            throws RemoteException;

    String[] getPackagesForUid(int uid) throws RemoteException;

    boolean hasSystemFeature(String name, int version) throws RemoteException;

    int checkPermission(String permName, String pkgName, int userId) throws RemoteException;

    void notifyDexLoad(String loadingPackageName, java.util.Map<String, String> classLoaderContextMap,
            String loaderIsa) throws RemoteException;

    abstract class Stub extends android.os.Binder implements IPackageManager {
        public static IPackageManager asInterface(IBinder obj) {
            throw new UnsupportedOperationException("stub");
        }
    }
}
