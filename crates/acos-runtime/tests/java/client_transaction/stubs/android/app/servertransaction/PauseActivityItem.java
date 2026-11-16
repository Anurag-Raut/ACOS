package android.app.servertransaction;

import android.os.Parcel;

/**
 * Signature-only stub for the {@code @hide}
 * {@code android.app.servertransaction.PauseActivityItem}.
 */
public class PauseActivityItem extends ActivityLifecycleItem {
    public static PauseActivityItem obtain(boolean finished, boolean userLeaving,
            int configChanges, boolean dontReport, boolean autoEnteringPip) {
        throw new UnsupportedOperationException("stub");
    }

    public static final android.os.Parcelable.Creator<PauseActivityItem> CREATOR = null;

    public int describeContents() {
        return 0;
    }

    public void writeToParcel(Parcel dest, int flags) {
        throw new UnsupportedOperationException("stub");
    }
}
