package dev.acos.fixtures.pmsparcel;

import android.app.Activity;
import android.content.pm.PackageInfo;
import android.content.pm.PackageManager;
import android.os.Bundle;
import android.util.Log;
import android.widget.TextView;

/**
 * REL3P fixture: the reply of
 * {@code getPackageInfo(own package, GET_SIGNATURES|GET_META_DATA|GET_PERMISSIONS)}
 * -- {@code 0x100024c0}, the flags {@code WebViewFactory} uses -- must decode in
 * the framework's own {@code PackageInfo.CREATOR} without an exception.
 *
 * <p>{@code PackageInfo.writeToParcel} writes {@code splitNames} and
 * {@code requestedPermissions} with {@code writeString8Array} (length in UTF-8
 * bytes). Before REL3P ACOS wrote them as String16 arrays, so every field after
 * {@code requestedPermissions} was read from the middle of the string payload:
 * the framework built a {@code Signature[]} of 7 209 071 entries and threw
 * {@code OutOfMemoryError: Failed to allocate a 7676737936 byte allocation at
 * Parcel.createString8Array}. Any app calling this method with those flags hit
 * it, so the check is an app-visible one, not a unit test.
 *
 * <p>Observable behaviour:
 * <ul>
 *   <li>{@code TextView id=report} = {@code report sigs=N perms=N splits=N
 *       meta=Y/N pkg=<name>} after the call, or {@code FAIL <exception>} when
 *       the reply cannot be decoded;</li>
 *   <li>log tag {@code AcosPmsParcel}: {@code getPackageInfo flags=0x100024c0
 *       sigs=N perms=N splits=N meta=Y/N} followed by {@code OK}.</li>
 * </ul>
 */
public class MainActivity extends Activity {
    private static final String TAG = "AcosPmsParcel";

    /** GET_SIGNATURES|GET_META_DATA|GET_PERMISSIONS, as WebViewFactory asks. */
    private static final int FLAGS = 0x100024c0;

    @Override
    protected void onCreate(Bundle savedInstanceState) {
        super.onCreate(savedInstanceState);
        setContentView(R.layout.main);
        TextView report = (TextView) findViewById(R.id.report);
        Log.i(TAG, "onCreate");
        PackageManager pm = getPackageManager();
        try {
            PackageInfo pi = pm.getPackageInfo(getPackageName(), FLAGS);
            int sigs = pi.signatures == null ? -1 : pi.signatures.length;
            int perms =
                    pi.requestedPermissions == null ? -1 : pi.requestedPermissions.length;
            int splits = pi.splitNames == null ? -1 : pi.splitNames.length;
            boolean meta = pi.applicationInfo != null && pi.applicationInfo.metaData != null;
            // Touch the fields that follow the String8 arrays in
            // `PackageInfo.writeToParcel`: a desynchronised reader lands in the
            // middle of a string here instead of on the next field.
            int appFlags = pi.applicationInfo == null ? 0 : pi.applicationInfo.flags;
            String appClass =
                    pi.applicationInfo == null
                            ? "null"
                            : String.valueOf(pi.applicationInfo.className);
            Log.i(
                    TAG,
                    "getPackageInfo flags=0x"
                            + Integer.toHexString(FLAGS)
                            + " sigs="
                            + sigs
                            + " perms="
                            + perms
                            + " splits="
                            + splits
                            + " meta="
                            + (meta ? "Y" : "N")
                            + " appFlags="
                            + appFlags
                            + " appClass="
                            + appClass);
            report.setText(
                    "report sigs="
                            + sigs
                            + " perms="
                            + perms
                            + " splits="
                            + splits
                            + " meta="
                            + (meta ? "Y" : "N")
                            + " pkg="
                            + pi.packageName);
            Log.i(TAG, "OK");
        } catch (Throwable t) {
            // The pre-REL3P failure is an OutOfMemoryError inside the parcel
            // reader, so catch Throwable, not just Exception: the point of the
            // fixture is to surface it on screen instead of crashing.
            Log.e(TAG, "FAIL " + t, t);
            report.setText("FAIL " + t);
        }
    }
}
