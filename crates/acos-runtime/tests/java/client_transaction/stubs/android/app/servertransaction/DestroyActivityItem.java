package android.app.servertransaction;

import android.os.Parcel;

/**
 * Signature-only stub for the {@code @hide}
 * {@code android.app.servertransaction.DestroyActivityItem}.
 */
public class DestroyActivityItem extends ActivityLifecycleItem {
    public static DestroyActivityItem obtain(boolean finished, int configChanges) {
        throw new UnsupportedOperationException("stub");
    }

    public static final android.os.Parcelable.Creator<DestroyActivityItem> CREATOR = null;

    public int describeContents() {
        return 0;
    }

    public void writeToParcel(Parcel dest, int flags) {
        throw new UnsupportedOperationException("stub");
    }
}
