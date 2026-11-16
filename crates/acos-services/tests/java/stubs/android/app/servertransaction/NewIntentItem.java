package android.app.servertransaction;

import android.os.Parcel;
import java.util.List;

/**
 * Signature-only stub for the {@code @hide}
 * {@code android.app.servertransaction.NewIntentItem}.
 */
public class NewIntentItem extends ClientTransactionItem {
    public static NewIntentItem obtain(List intents, boolean resume) {
        throw new UnsupportedOperationException("stub");
    }

    public static final android.os.Parcelable.Creator<NewIntentItem> CREATOR = null;

    public int describeContents() {
        return 0;
    }

    public void writeToParcel(Parcel dest, int flags) {
        throw new UnsupportedOperationException("stub");
    }
}
