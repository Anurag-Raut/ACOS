package android.webkit;

// COMPILE-TIME ONLY stub: `WebViewProviderResponse` is a hidden framework class
// (it is not in the SDK `android.jar`), so the parcel test needs its erased
// shape to compile. At run time the **image's** class is the one that parses the
// reply, which is what the WV2 regression test is about: the signature here
// mirrors the image's `classes4.dex` field names and creator.
public final class WebViewProviderResponse implements android.os.Parcelable {
    public android.content.pm.PackageInfo packageInfo;
    public int status;

    @Override
    public int describeContents() {
        return 0;
    }

    @Override
    public void writeToParcel(android.os.Parcel dest, int flags) {
        dest.writeTypedObject(packageInfo, flags);
        dest.writeInt(status);
    }

    public static final android.os.Parcelable.Creator<WebViewProviderResponse> CREATOR =
            new android.os.Parcelable.Creator<WebViewProviderResponse>() {
                @Override
                public WebViewProviderResponse createFromParcel(android.os.Parcel source) {
                    throw new AssertionError("stub: the image's class must be used");
                }

                @Override
                public WebViewProviderResponse[] newArray(int size) {
                    throw new AssertionError("stub");
                }
            };
}
