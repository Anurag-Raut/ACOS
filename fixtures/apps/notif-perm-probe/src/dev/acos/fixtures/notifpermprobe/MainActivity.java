package dev.acos.fixtures.notifpermprobe;

import android.app.Activity;
import android.app.Notification;
import android.app.NotificationChannel;
import android.app.NotificationManager;
import android.app.PendingIntent;
import android.content.Intent;
import android.graphics.drawable.Icon;
import android.os.Bundle;
import android.util.Log;
import android.widget.TextView;

/**
 * notif-perm-probe: the real framework notification path, end to end.
 *
 * <p>Plain framework APIs only (no AndroidX). On start it
 * <ol>
 *   <li>creates a notification channel ({@code NotificationManager.createNotificationChannel}),
 *   <li>posts one notification with a {@code PendingIntent.getActivity} content
 *       intent and one action ({@code Notification.Action} with its own
 *       PendingIntent),
 *   <li>shows {@code areNotificationsEnabled()} and what it asked for.
 * </ol>
 *
 * <p>Nothing is caught: a stack that cannot serve {@code getIntentSender} /
 * {@code enqueueNotificationWithTag} crashes the app, which is the honest
 * signal a scenario should see. {@code areNotificationsEnabled()} is read
 * <b>before</b> the post too, so the two values are both on screen — after a
 * denial AOSP still answers {@code false}, the app just cannot see the dropped
 * notification (the module records it).
 */
public class MainActivity extends Activity {
    private static final String TAG = "AcosNotifProbe";

    /** The channel the notification is posted into. */
    public static final String CHANNEL_ID = "probe";
    /** {@code notify(tag, id, notification)}. */
    public static final String NOTIFICATION_TAG = "probe-notification";
    /** The notification id the module records. */
    public static final int NOTIFICATION_ID = 7;
    /** The content intent's action (the sender evidence carries it). */
    public static final String ACTION_OPEN = "dev.acos.fixtures.notifpermprobe.OPEN";
    /** The action button's action. */
    public static final String ACTION_BUTTON = "dev.acos.fixtures.notifpermprobe.ACTION";

    private TextView enabledView;
    private TextView postedView;

    @Override
    protected void onCreate(Bundle savedInstanceState) {
        super.onCreate(savedInstanceState);
        Log.i(TAG, "onCreate");
        setContentView(R.layout.main);
        enabledView = (TextView) findViewById(R.id.enabled);
        postedView = (TextView) findViewById(R.id.posted);
        NotificationManager notifications = getSystemService(NotificationManager.class);

        // 1. The channel (`createNotificationChannels`, a ParceledListSlice).
        notifications.createNotificationChannel(
                new NotificationChannel(CHANNEL_ID, "Probe", NotificationManager.IMPORTANCE_DEFAULT));
        ((TextView) findViewById(R.id.channel)).setText("channel: " + CHANNEL_ID);
        Log.i(TAG, "channel created id=" + CHANNEL_ID);

        boolean enabledBefore = notifications.areNotificationsEnabled();
        Log.i(TAG, "areNotificationsEnabled=" + enabledBefore);
        showEnabled(enabledBefore);

        // 2. The two PendingIntents: a content intent and one action intent, both
        //    explicit `getActivity` senders (FLAG_IMMUTABLE: a tap passes no
        //    fill-in; a mutable fill-in is a named ACOS gap).
        Intent target = new Intent(this, TargetActivity.class)
                .setAction(ACTION_OPEN)
                .addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
                .putExtra("from", "content");
        PendingIntent contentIntent = PendingIntent.getActivity(
                this, 1, target, PendingIntent.FLAG_UPDATE_CURRENT | PendingIntent.FLAG_IMMUTABLE);
        Intent actionTarget = new Intent(this, TargetActivity.class)
                .setAction(ACTION_BUTTON)
                .addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
                .putExtra("from", "action");
        PendingIntent actionIntent = PendingIntent.getActivity(
                this, 2, actionTarget, PendingIntent.FLAG_UPDATE_CURRENT | PendingIntent.FLAG_IMMUTABLE);

        Notification notification = new Notification.Builder(this, CHANNEL_ID)
                .setSmallIcon(android.R.drawable.ic_dialog_info)
                .setContentTitle("Probe title")
                .setContentText("Probe text")
                .setContentIntent(contentIntent)
                .addAction(new Notification.Action.Builder(
                                Icon.createWithResource(this, android.R.drawable.ic_menu_view),
                                "Open",
                                actionIntent)
                        .build())
                .build();

        // 3. Post it. A denied POST_NOTIFICATIONS is *not* an exception: the
        //    system drops the notification and records why (the module's
        //    `dropped[]`), exactly like a device.
        notifications.notify(NOTIFICATION_TAG, NOTIFICATION_ID, notification);
        Log.i(TAG, "notified tag=" + NOTIFICATION_TAG + " id=" + NOTIFICATION_ID);
        postedView.setText("posted: id " + NOTIFICATION_ID);

        // 4. Read the gate again (AOSP answers the same after a drop).
        boolean enabledAfter = notifications.areNotificationsEnabled();
        Log.i(TAG, "areNotificationsEnabled after notify=" + enabledAfter);
        showEnabled(enabledAfter);
    }

    private void showEnabled(boolean enabled) {
        enabledView.setText("enabled: " + enabled);
    }
}
