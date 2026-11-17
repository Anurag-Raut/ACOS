package android.app.servertransaction;

import android.app.ActivityOptions;
import android.app.IActivityClientController;
import android.app.ProfilerInfo;
import android.content.Intent;
import android.content.pm.ActivityInfo;
import android.content.res.Configuration;
import android.os.Bundle;
import android.os.IBinder;
import android.os.Parcel;
import android.os.PersistableBundle;
import com.android.internal.app.IVoiceInteractor;
import java.util.List;

/**
 * Signature-only stub for the {@code @hide}
 * {@code android.app.servertransaction.LaunchActivityItem}: the image's own
 * 21-parameter {@code obtain} (no {@code sceneTransitionInfo},
 * {@code initialCallerInfoAccessToken} or {@code activityWindowInfo} on this
 * build).
 */
public class LaunchActivityItem extends ClientTransactionItem {
    public static LaunchActivityItem obtain(Intent intent, int ident, ActivityInfo info,
            Configuration curConfig, Configuration overrideConfig, int deviceId, String referrer,
            IVoiceInteractor voiceInteractor, int procState, Bundle state,
            PersistableBundle persistentState, List pendingResults, List pendingNewIntents,
            ActivityOptions activityOptions, boolean isForward, ProfilerInfo profilerInfo,
            IBinder assistToken, IActivityClientController activityClientController,
            IBinder shareableActivityToken, boolean launchedFromBubble, IBinder taskFragmentToken) {
        throw new UnsupportedOperationException("stub");
    }

    public static final android.os.Parcelable.Creator<LaunchActivityItem> CREATOR = null;

    public int describeContents() {
        return 0;
    }

    public void writeToParcel(Parcel dest, int flags) {
        throw new UnsupportedOperationException("stub");
    }
}
