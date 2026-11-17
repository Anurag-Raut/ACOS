package android.app;

/**
 * Signature-only stub for the {@code @hide} {@code android.app.ResultInfo}.
 */
public class ResultInfo implements android.os.Parcelable {
    public ResultInfo(String resultWho, int requestCode, int resultCode,
            android.content.Intent data) {
        throw new UnsupportedOperationException("stub");
    }

    public static final android.os.Parcelable.Creator<ResultInfo> CREATOR = null;

    public int describeContents() {
        return 0;
    }

    public void writeToParcel(android.os.Parcel dest, int flags) {
        throw new UnsupportedOperationException("stub");
    }
}
