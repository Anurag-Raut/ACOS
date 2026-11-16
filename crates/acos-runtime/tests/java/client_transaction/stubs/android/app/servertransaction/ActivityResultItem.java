package android.app.servertransaction;

import android.os.Parcel;
import java.util.List;

/**
 * Signature-only stub for the {@code @hide}
 * {@code android.app.servertransaction.ActivityResultItem}.
 */
public class ActivityResultItem extends ClientTransactionItem {
    public static ActivityResultItem obtain(List resultInfoList) {
        throw new UnsupportedOperationException("stub");
    }

    public static final android.os.Parcelable.Creator<ActivityResultItem> CREATOR = null;

    public int describeContents() {
        return 0;
    }

    public void writeToParcel(Parcel dest, int flags) {
        throw new UnsupportedOperationException("stub");
    }
}
