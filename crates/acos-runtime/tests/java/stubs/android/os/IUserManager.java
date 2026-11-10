package android.os;

import android.content.pm.UserInfo;

/**
 * Signature-only stub for the {@code @hide} {@code android.os.IUserManager}.
 * The real interface + {@code $Stub$Proxy} come from the boot-classpath framework
 * DEX at runtime; this stub exists only so {@code ImeClipboardMain} compiles.
 */
public interface IUserManager {
    UserInfo getUserInfo(int userId) throws RemoteException;

    java.util.List getUsers(boolean excludeDying, boolean excludePartial,
            boolean excludeHidden) throws RemoteException;

    abstract class Stub extends android.os.Binder implements IUserManager {
        public static IUserManager asInterface(IBinder obj) {
            throw new UnsupportedOperationException("stub");
        }
    }
}
