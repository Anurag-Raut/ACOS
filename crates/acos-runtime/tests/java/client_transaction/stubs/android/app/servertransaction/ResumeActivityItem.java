package android.app.servertransaction;

import android.os.Parcel;

/**
 * Signature-only stub for the {@code @hide}
 * {@code android.app.servertransaction.ResumeActivityItem}.
 */
public class ResumeActivityItem extends ActivityLifecycleItem {
    // The image's own overloads: `obtain(IZZ)` (sets `mUpdateProcState = true`)
    // and `obtain(ZZ)` (`PROCESS_STATE_UNKNOWN`, no update). There is no
    // `obtain(IZZZ)` on this build.
    public static ResumeActivityItem obtain(int procState, boolean isForward,
            boolean shouldSendCompatFakeFocus) {
        throw new UnsupportedOperationException("stub");
    }

    public static ResumeActivityItem obtain(boolean isForward,
            boolean shouldSendCompatFakeFocus) {
        throw new UnsupportedOperationException("stub");
    }

    public static final android.os.Parcelable.Creator<ResumeActivityItem> CREATOR = null;

    public int describeContents() {
        return 0;
    }

    public void writeToParcel(Parcel dest, int flags) {
        throw new UnsupportedOperationException("stub");
    }
}
