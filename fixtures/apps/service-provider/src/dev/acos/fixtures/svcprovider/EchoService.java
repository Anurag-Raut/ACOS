package dev.acos.fixtures.svcprovider;

import android.app.Service;
import android.content.ContentValues;
import android.content.Intent;
import android.os.IBinder;
import android.util.Log;

/**
 * Started {@link Service} forming the first leg of the round trip: it receives
 * the payload extra, writes it through {@link EchoProvider} and broadcasts it
 * back. Plain framework APIs only.
 */
public class EchoService extends Service {
    public static final String EXTRA_PAYLOAD = "payload";
    public static final String ACTION_ECHO = "dev.acos.fixtures.svcprovider.ECHO";

    private static final String TAG = "AcosSvc";

    @Override
    public void onCreate() {
        super.onCreate();
        Log.i(TAG, "service.onCreate");
    }

    @Override
    public int onStartCommand(Intent intent, int flags, int startId) {
        String payload = intent == null ? "" : intent.getStringExtra(EXTRA_PAYLOAD);
        if (payload == null) {
            payload = "";
        }
        Log.i(TAG, "service.onStartCommand id=" + startId + " payload=" + payload);

        ContentValues values = new ContentValues();
        values.put("payload", payload);
        getContentResolver().insert(EchoProvider.CONTENT_URI, values);

        Intent echo = new Intent(ACTION_ECHO);
        echo.setPackage(getPackageName());
        echo.putExtra(EXTRA_PAYLOAD, payload);
        sendBroadcast(echo);
        Log.i(TAG, "service.broadcast sent payload=" + payload);
        return START_NOT_STICKY;
    }

    @Override
    public void onDestroy() {
        Log.i(TAG, "service.onDestroy");
        super.onDestroy();
    }

    @Override
    public IBinder onBind(Intent intent) {
        Log.i(TAG, "service.onBind");
        return null;
    }
}
