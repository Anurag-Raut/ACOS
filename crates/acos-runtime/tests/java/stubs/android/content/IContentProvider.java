package android.content;

import android.os.Bundle;
import android.os.RemoteException;

/**
 * Signature-only stub for the {@code @hide} {@code android.content.IContentProvider}.
 * The real interface + the hand-written {@code ContentProviderProxy} come from
 * the boot-classpath framework DEX at runtime.
 */
public interface IContentProvider {
    Bundle call(AttributionSource attributionSource, String method, String arg,
            String authority, Bundle extras) throws RemoteException;
}
