package android.content;

import android.os.Bundle;
import android.os.IBinder;
import android.os.RemoteException;

/**
 * Signature-only stub for the {@code @hide}, package-private
 * {@code android.content.ContentProviderProxy} (the hand-written
 * {@code IContentProvider} client used by {@code Settings$NameValueCache}).
 * The real class lives in the boot-classpath framework DEX and is loaded at
 * runtime; this stub only lets the test compile.
 */
public final class ContentProviderProxy implements IContentProvider {
    public ContentProviderProxy(IBinder remote) {
        throw new UnsupportedOperationException("stub");
    }

    @Override
    public Bundle call(AttributionSource attributionSource, String method, String arg,
            String authority, Bundle extras) throws RemoteException {
        throw new UnsupportedOperationException("stub");
    }
}
