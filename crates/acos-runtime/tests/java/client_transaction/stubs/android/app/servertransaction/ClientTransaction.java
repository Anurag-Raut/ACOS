package android.app.servertransaction;

import android.app.IApplicationThread;
import android.os.IBinder;
import android.os.Parcel;

/**
 * Signature-only stub for the {@code @hide}
 * {@code android.app.servertransaction.ClientTransaction}.
 */
public class ClientTransaction implements android.os.Parcelable {
    public static ClientTransaction obtain(IApplicationThread client, IBinder activityToken) {
        throw new UnsupportedOperationException("stub");
    }

    public void addCallback(ClientTransactionItem item) {
        throw new UnsupportedOperationException("stub");
    }

    public void setLifecycleStateRequest(ActivityLifecycleItem item) {
        throw new UnsupportedOperationException("stub");
    }

    public static final android.os.Parcelable.Creator<ClientTransaction> CREATOR = null;

    public int describeContents() {
        return 0;
    }

    public void writeToParcel(Parcel dest, int flags) {
        throw new UnsupportedOperationException("stub");
    }
}
