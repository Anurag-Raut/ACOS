package android.util;

/**
 * Signature-only stub for the {@code @hide} {@code android.util.MemoryIntArray}.
 * The real class (and its `nativeOpen`/`nativeGet` natives) come from the
 * boot-classpath framework DEX at runtime.
 */
public final class MemoryIntArray implements android.os.Parcelable {
    public static final android.os.Parcelable.Creator<MemoryIntArray> CREATOR =
            new android.os.Parcelable.Creator<MemoryIntArray>() {
                @Override
                public MemoryIntArray createFromParcel(android.os.Parcel source) {
                    throw new UnsupportedOperationException("stub");
                }

                @Override
                public MemoryIntArray[] newArray(int size) {
                    throw new UnsupportedOperationException("stub");
                }
            };

    public MemoryIntArray(int size) throws java.io.IOException {
        throw new UnsupportedOperationException("stub");
    }

    public int get(int index) {
        throw new UnsupportedOperationException("stub");
    }

    public void set(int index, int value) {
        throw new UnsupportedOperationException("stub");
    }

    public int size() {
        throw new UnsupportedOperationException("stub");
    }

    public void close() {
        throw new UnsupportedOperationException("stub");
    }

    @Override
    public int describeContents() {
        return 0;
    }

    @Override
    public void writeToParcel(android.os.Parcel dest, int flags) {
        throw new UnsupportedOperationException("stub");
    }
}
