package com.android.internal.content;

/**
 * Signature-only stub for the {@code @hide}
 * {@code com.android.internal.content.ReferrerIntent} (an {@code Intent} plus
 * the referring package).
 */
public class ReferrerIntent extends android.content.Intent {
    public ReferrerIntent(android.content.Intent originalIntent, String referrer) {
        throw new UnsupportedOperationException("stub");
    }

    public static final android.os.Parcelable.Creator<ReferrerIntent> CREATOR = null;
}
