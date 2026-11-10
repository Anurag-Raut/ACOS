package dev.acos.fixtures.svcprovider;

import android.content.BroadcastReceiver;
import android.content.Context;
import android.content.Intent;
import android.database.Cursor;
import android.util.Log;

/**
 * Manifest-declared {@link BroadcastReceiver} closing the round trip: it sees
 * the broadcast sent by {@link EchoService} and queries {@link EchoProvider} to
 * confirm the write landed. Plain framework APIs only.
 */
public class EchoReceiver extends BroadcastReceiver {
    private static final String TAG = "AcosSvc";

    @Override
    public void onReceive(Context context, Intent intent) {
        String action = intent.getAction();
        String payload = intent.getStringExtra(EchoService.EXTRA_PAYLOAD);
        if (payload == null) {
            payload = "";
        }
        Log.i(TAG, "receiver.onReceive action=" + action + " payload=" + payload);

        int count = 0;
        Cursor cursor = context.getContentResolver().query(EchoProvider.CONTENT_URI,
                new String[] {"_id", "payload"}, null, null, null);
        if (cursor != null) {
            try {
                count = cursor.getCount();
            } finally {
                cursor.close();
            }
        }
        Log.i(TAG, "receiver.provider rows=" + count);
    }
}
