package android.app.servertransaction;

import android.os.Parcel;

/**
 * Signature-only stub for the {@code @hide}
 * {@code android.app.servertransaction.ClientTransactionItem}. The runtime
 * binds the real framework class from the boot-classpath DEX.
 */
public abstract class ClientTransactionItem implements android.os.Parcelable {
    public abstract void writeToParcel(Parcel dest, int flags);
}
