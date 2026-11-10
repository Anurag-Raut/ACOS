package android.content;

import android.os.IBinder;
import android.os.RemoteException;

/**
 * Signature-only stub for the {@code @hide} {@code android.content.IClipboard}.
 * The real interface + {@code $Stub$Proxy} come from the boot-classpath
 * framework DEX at runtime; this stub exists only so {@code ImeClipboardMain}
 * compiles.
 */
public interface IClipboard {
    void setPrimaryClip(ClipData clip, String callingPackage, String attributionTag,
            int userId, int deviceId) throws RemoteException;

    void setPrimaryClipAsPackage(ClipData clip, String callingPackage,
            String attributionTag, int userId, int deviceId, String sourcePackage)
            throws RemoteException;

    void clearPrimaryClip(String callingPackage, String attributionTag, int userId,
            int deviceId) throws RemoteException;

    ClipData getPrimaryClip(String callingPackage, String attributionTag, int userId,
            int deviceId) throws RemoteException;

    ClipDescription getPrimaryClipDescription(String callingPackage, String attributionTag,
            int userId, int deviceId) throws RemoteException;

    boolean hasPrimaryClip(String callingPackage, String attributionTag, int userId,
            int deviceId) throws RemoteException;

    void addPrimaryClipChangedListener(IOnPrimaryClipChangedListener listener,
            String callingPackage, String attributionTag, int userId, int deviceId)
            throws RemoteException;

    void removePrimaryClipChangedListener(IOnPrimaryClipChangedListener listener,
            String callingPackage, String attributionTag, int userId, int deviceId)
            throws RemoteException;

    abstract class Stub extends android.os.Binder implements IClipboard {
        public static IClipboard asInterface(IBinder obj) {
            throw new UnsupportedOperationException("stub");
        }
    }
}
