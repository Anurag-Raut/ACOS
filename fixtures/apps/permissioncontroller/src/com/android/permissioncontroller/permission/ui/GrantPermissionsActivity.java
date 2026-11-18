package com.android.permissioncontroller.permission.ui;

import android.app.Activity;
import android.content.Intent;
import android.content.pm.PackageManager;
import android.content.pm.PermissionGroupInfo;
import android.content.pm.PermissionInfo;
import android.os.Bundle;
import android.util.Log;
import android.view.View;
import android.widget.Button;
import android.widget.CheckBox;
import android.widget.TextView;

import com.android.permissioncontroller.R;

/**
 * SYSUI1: the permission dialog, as the guest system app the framework's own
 * request path names.
 *
 * <p>{@code Activity.requestPermissions} builds
 * {@code ACTION_REQUEST_PERMISSIONS} addressed to
 * {@code com.android.permissioncontroller}
 * ({@code PackageManager.buildRequestPermissionsIntent},
 * {@code PackageManager.java:7049}) and starts it for a result. This activity is
 * what the host resolves that intent to; it runs in its own guest process and
 * answers with the AOSP result shape:
 *
 * <ul>
 *   <li>{@code EXTRA_REQUEST_PERMISSIONS_NAMES} — the requested permissions, and
 *   <li>{@code EXTRA_REQUEST_PERMISSIONS_RESULTS} — one
 *       {@code PERMISSION_GRANTED}/{@code PERMISSION_DENIED} per name,
 * </ul>
 *
 * which {@code Activity.dispatchRequestPermissionsResult}
 * ({@code Activity.java:9480}) turns into
 * {@code onRequestPermissionsResult(requestCode, permissions, grantResults)}.
 *
 * <p>A real controller asks {@code IPermissionManager} for the state it cannot
 * query itself. This app cannot reach the hidden interface, so the host passes
 * the same facts in the launch ({@code acos.permission.GRANTED} /
 * {@code acos.permission.FIXED}), and the dialog does what
 * {@code GrantPermissionsActivity} does with them: already-granted permissions
 * are granted again, "don't ask again" ones are denied without asking, and when
 * nothing is left to ask the dialog returns without drawing.
 *
 * <p>"Don't ask again" is carried back in
 * {@code android.content.pm.extra.REQUEST_PERMISSIONS_NEVER_ASK_AGAIN}; the host
 * applies {@code FLAG_PERMISSION_USER_FIXED} for it (a device's controller calls
 * {@code updatePermissionFlags} itself).
 */
public class GrantPermissionsActivity extends Activity {
    private static final String TAG = "AcosPermController";

    /** {@code PackageManager.EXTRA_REQUEST_PERMISSIONS_NAMES}. */
    public static final String EXTRA_NAMES = "android.content.pm.extra.REQUEST_PERMISSIONS_NAMES";
    /** {@code PackageManager.EXTRA_REQUEST_PERMISSIONS_RESULTS}. */
    public static final String EXTRA_RESULTS = "android.content.pm.extra.REQUEST_PERMISSIONS_RESULTS";
    /** ACOS system-app result contract: the "Don't ask again" answer. */
    public static final String EXTRA_NEVER_ASK_AGAIN =
            "android.content.pm.extra.REQUEST_PERMISSIONS_NEVER_ASK_AGAIN";
    /** ACOS system-app launch contract: permissions already granted. */
    public static final String EXTRA_GRANTED = "acos.permission.GRANTED";
    /** ACOS system-app launch contract: permissions denied "don't ask again". */
    public static final String EXTRA_FIXED = "acos.permission.FIXED";
    /** ACOS system-app launch contract: the requesting package. */
    public static final String EXTRA_CALLING_PACKAGE = "acos.permission.CALLING_PACKAGE";

    private String[] permissions = new String[0];
    private String[] granted = new String[0];
    private String[] fixed = new String[0];
    private CheckBox dontAskAgain;

    @Override
    protected void onCreate(Bundle savedInstanceState) {
        super.onCreate(savedInstanceState);
        Intent request = getIntent();
        permissions = strings(request.getStringArrayExtra(EXTRA_NAMES));
        granted = strings(request.getStringArrayExtra(EXTRA_GRANTED));
        fixed = strings(request.getStringArrayExtra(EXTRA_FIXED));
        String callingPackage = request.getStringExtra(EXTRA_CALLING_PACKAGE);
        Log.i(TAG, "created names=" + names() + " granted=" + join(granted) + " fixed=" + join(fixed)
                + " package=" + callingPackage);

        int[] auto = autoResults();
        // `allRequestedPermissionsGranted`: when nothing is left to ask the real
        // controller grants and finishes without drawing anything.
        if (!hasOpenQuestion()) {
            Log.i(TAG, "nothing to ask, returning results " + describe(auto));
            answer(auto, false);
            return;
        }

        setContentView(R.layout.permission_dialog);
        ((TextView) findViewById(R.id.title)).setText(
                callingPackage == null ? getString(R.string.request_title)
                        : "Allow " + callingPackage + " to access?");
        int[] rowIds = {R.id.permission_0, R.id.permission_1};
        for (int i = 0; i < permissions.length && i < rowIds.length; i++) {
            TextView row = (TextView) findViewById(rowIds[i]);
            row.setText(label(permissions[i]) + autoSuffix(permissions[i]));
            row.setVisibility(View.VISIBLE);
        }
        dontAskAgain = (CheckBox) findViewById(R.id.dont_ask_again);
        Button allow = (Button) findViewById(R.id.allow_button);
        Button deny = (Button) findViewById(R.id.deny_button);
        allow.setOnClickListener(new View.OnClickListener() {
            @Override
            public void onClick(View view) {
                onAnswer(true);
            }
        });
        deny.setOnClickListener(new View.OnClickListener() {
            @Override
            public void onClick(View view) {
                onAnswer(false);
            }
        });
    }

    private void onAnswer(boolean allow) {
        boolean neverAskAgain = !allow && dontAskAgain != null && dontAskAgain.isChecked();
        int[] results = new int[permissions.length];
        for (int i = 0; i < permissions.length; i++) {
            if (isAutoGranted(permissions[i])) {
                results[i] = PackageManager.PERMISSION_GRANTED;
            } else if (isAutoDenied(permissions[i])) {
                results[i] = PackageManager.PERMISSION_DENIED;
            } else {
                results[i] = allow ? PackageManager.PERMISSION_GRANTED : PackageManager.PERMISSION_DENIED;
            }
        }
        Log.i(TAG, (allow ? "allow" : "deny") + " names=" + names() + " results=" + describe(results)
                + " never_ask_again=" + neverAskAgain);
        answer(results, neverAskAgain);
    }

    /** The AOSP result shape plus the "don't ask again" flag. */
    private void answer(int[] results, boolean neverAskAgain) {
        Intent result = new Intent();
        result.putExtra(EXTRA_NAMES, permissions);
        result.putExtra(EXTRA_RESULTS, results);
        if (neverAskAgain) {
            result.putExtra(EXTRA_NEVER_ASK_AGAIN, true);
        }
        setResult(RESULT_OK, result);
        finish();
    }

    private int[] autoResults() {
        int[] results = new int[permissions.length];
        for (int i = 0; i < permissions.length; i++) {
            results[i] = isAutoDenied(permissions[i])
                    ? PackageManager.PERMISSION_DENIED
                    : PackageManager.PERMISSION_GRANTED;
        }
        return results;
    }

    private boolean hasOpenQuestion() {
        for (String permission : permissions) {
            if (!isAutoGranted(permission) && !isAutoDenied(permission)) {
                return true;
            }
        }
        return false;
    }

    private boolean isAutoGranted(String permission) {
        return contains(granted, permission);
    }

    private boolean isAutoDenied(String permission) {
        return contains(fixed, permission);
    }

    private String autoSuffix(String permission) {
        if (isAutoGranted(permission)) {
            return " (already allowed)";
        }
        if (isAutoDenied(permission)) {
            return " (denied, don't ask again)";
        }
        return "";
    }

    /**
     * The permission's label, as AOSP's own `PermissionInfo.loadLabel` answers
     * it. SYSUI3B: `IPermissionManager.getPermissionInfo` now carries
     * `nonLocalizedLabel`, so `loadLabel` returns the text the platform declares
     * for the permission ("take pictures and videos" for CAMERA) with no
     * resource resolution. `loadLabel` only needs the defining package's
     * resources when the label is a resource id, which a non-localized label
     * never is.
     *
     * <p>The group lookup runs too: it is the second half of the store
     * (`getPermissionGroupInfo`), and a device's dialog shows the group title
     * next to the permission. Its failure never hides the label.
     *
     * <p>The last-segment fallback is what a phone shows for a permission that
     * is not in the platform catalog and that the app did not declare: it is
     * kept, and logged, so an unknown permission is visible in the evidence
     * instead of being silently disguised as a real label.
     */
    private String label(String permission) {
        try {
            PermissionInfo info = getPackageManager().getPermissionInfo(permission, 0);
            CharSequence label = info.loadLabel(getPackageManager());
            if (label != null && label.length() > 0) {
                Log.i(TAG, "permission " + permission + " label=" + label
                        + " group=" + groupLabel(info));
                return label.toString();
            }
        } catch (Throwable ignored) {
            Log.i(TAG, "label fallback for " + permission + ": " + ignored);
        }
        int dot = permission.lastIndexOf('.');
        return dot < 0 ? permission : permission.substring(dot + 1);
    }

    /** The `PermissionInfo.group` title from `getPermissionGroupInfo`. */
    private String groupLabel(PermissionInfo info) {
        String group = info.group;
        if (group == null || group.length() == 0) {
            return "none";
        }
        try {
            PermissionGroupInfo groupInfo = getPackageManager().getPermissionGroupInfo(group, 0);
            CharSequence label = groupInfo.loadLabel(getPackageManager());
            if (label != null && label.length() > 0) {
                return label.toString();
            }
        } catch (Throwable ignored) {
            Log.i(TAG, "group fallback for " + group + ": " + ignored);
        }
        return group;
    }

    private static boolean contains(String[] values, String value) {
        for (String item : values) {
            if (item != null && item.equals(value)) {
                return true;
            }
        }
        return false;
    }

    private static String[] strings(String[] values) {
        return values == null ? new String[0] : values;
    }

    private String names() {
        return join(permissions);
    }

    private static String join(String[] values) {
        StringBuilder text = new StringBuilder();
        for (int i = 0; i < values.length; i++) {
            if (i > 0) {
                text.append(',');
            }
            text.append(values[i]);
        }
        return text.toString();
    }

    private static String describe(int[] values) {
        StringBuilder text = new StringBuilder();
        for (int i = 0; i < values.length; i++) {
            if (i > 0) {
                text.append(',');
            }
            text.append(values[i] == PackageManager.PERMISSION_GRANTED ? "granted" : "denied");
        }
        return text.toString();
    }
}
