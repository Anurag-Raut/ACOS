package com.android.systemui;

import android.app.Activity;
import android.content.Intent;
import android.os.Bundle;
import android.util.Log;
import android.view.View;
import android.widget.TextView;

/**
 * SYSUI2: the notification shade, as the guest system app the notification
 * layer's {@code open_shade} control op starts.
 *
 * <p>A device's shade is a window in {@code com.android.systemui} that lists the
 * posted notifications and, when a row is tapped, fires that notification's
 * content {@code PendingIntent} and dismisses the record. This activity is that
 * window: the launches extras carry the list the system would bind
 * ({@code acos.shade.keys/titles/texts/actions}), each row is one notification,
 * and a tap answers with the row's key in the AOSP result shape
 * ({@link #EXTRA_TAPPED}). The host then fires the notification's own content
 * sender and cancels it — the work systemui's controller does in its own
 * process (the guest app cannot reach the hidden
 * {@code INotificationManager}/{@code IIntentSender} stubs, exactly as the
 * permission controller cannot reach {@code IPermissionManager}).
 */
public class ShadeActivity extends Activity {
    private static final String TAG = "AcosSystemUI";

    /** Launch contract: one key per row, {@code "<id>:<tag>"} ({@code tag} may be empty). */
    public static final String EXTRA_KEYS = "acos.shade.keys";
    /** Launch contract: one title per row ({@code android.title}). */
    public static final String EXTRA_TITLES = "acos.shade.titles";
    /** Launch contract: one text per row ({@code android.text}). */
    public static final String EXTRA_TEXTS = "acos.shade.texts";
    /** Launch contract: one comma-joined action-title list per row (may be empty). */
    public static final String EXTRA_ACTIONS = "acos.shade.actions";
    /** Result contract: the key of the row the user tapped. */
    public static final String EXTRA_TAPPED = "acos.shade.tapped";
    /** Result contract: the action index fired, or {@code -1} for the content intent. */
    public static final String EXTRA_ACTION = "acos.shade.action";

    private String[] keys = new String[0];
    private static final int[] ROW_IDS = {R.id.shade_row_0, R.id.shade_row_1, R.id.shade_row_2};

    @Override
    protected void onCreate(Bundle savedInstanceState) {
        super.onCreate(savedInstanceState);
        Intent launch = getIntent();
        keys = strings(launch.getStringArrayExtra(EXTRA_KEYS));
        String[] titles = strings(launch.getStringArrayExtra(EXTRA_TITLES));
        String[] texts = strings(launch.getStringArrayExtra(EXTRA_TEXTS));
        String[] actions = strings(launch.getStringArrayExtra(EXTRA_ACTIONS));
        Log.i(TAG, "shade created rows=" + keys.length + " keys=" + join(keys));

        setContentView(R.layout.shade);
        if (keys.length == 0) {
            findViewById(R.id.shade_empty).setVisibility(View.VISIBLE);
            return;
        }
        for (int i = 0; i < keys.length && i < ROW_IDS.length; i++) {
            final String key = keys[i];
            TextView row = (TextView) findViewById(ROW_IDS[i]);
            row.setText(rowText(at(titles, i), at(texts, i), at(actions, i)));
            row.setVisibility(View.VISIBLE);
            row.setOnClickListener(new View.OnClickListener() {
                @Override
                public void onClick(View view) {
                    onRow(key, -1);
                }
            });
        }
    }

    /** A row tap: answer which notification, exactly like the shade's own click handler. */
    private void onRow(String key, int action) {
        Log.i(TAG, "tapped key=" + key + " action=" + action);
        Intent result = new Intent();
        result.putExtra(EXTRA_TAPPED, key);
        result.putExtra(EXTRA_ACTION, action);
        setResult(RESULT_OK, result);
        finish();
    }

    /** {@code NotificationListenerService}-style row text: title — text · actions. */
    private static String rowText(String title, String text, String actions) {
        StringBuilder row = new StringBuilder(title);
        if (text.length() > 0) {
            if (row.length() > 0) row.append(" — ");
            row.append(text);
        }
        if (actions.length() > 0) {
            if (row.length() > 0) row.append(" · ");
            row.append(actions);
        }
        return row.toString();
    }

    private static String at(String[] values, int index) {
        return index < values.length && values[index] != null ? values[index] : "";
    }

    private static String[] strings(String[] values) {
        return values == null ? new String[0] : values;
    }

    private static String join(String[] values) {
        StringBuilder text = new StringBuilder();
        for (int i = 0; i < values.length; i++) {
            if (i > 0) text.append(',');
            text.append(values[i]);
        }
        return text.toString();
    }
}
