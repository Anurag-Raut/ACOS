package dev.acos.fixtures.svcprovider;

import android.app.Activity;
import android.content.BroadcastReceiver;
import android.content.Context;
import android.content.Intent;
import android.content.IntentFilter;
import android.os.Bundle;
import android.util.Log;
import android.view.View;
import android.widget.Button;
import android.widget.EditText;
import android.widget.TextView;

/**
 * Activity that starts the service -> provider -> receiver round trip. Plain
 * framework APIs only.
 *
 * Observable behaviour:
 *  - Button id=send starts {@link EchoService} with the EditText id=payload extra
 *  - the service inserts into {@link EchoProvider} and broadcasts
 *  - the dynamic receiver registered here updates TextView id=state
 *  - the manifest-declared {@link EchoReceiver} also observes the broadcast
 *  - lifecycle/actions log with tag "AcosSvc"
 */
public class MainActivity extends Activity {
    private static final String TAG = "AcosSvc";

    private EditText payload;
    private TextView state;
    private boolean registered = false;

    private final BroadcastReceiver uiReceiver = new BroadcastReceiver() {
        @Override
        public void onReceive(Context context, Intent intent) {
            String value = intent.getStringExtra(EchoService.EXTRA_PAYLOAD);
            Log.i(TAG, "activity.receiver payload=" + value);
            state.setText("echo: " + value);
        }
    };

    @Override
    protected void onCreate(Bundle savedInstanceState) {
        super.onCreate(savedInstanceState);
        Log.i(TAG, "onCreate");
        setContentView(R.layout.main);
        payload = (EditText) findViewById(R.id.payload);
        state = (TextView) findViewById(R.id.state);
        ((Button) findViewById(R.id.send)).setOnClickListener(new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                send();
            }
        });
    }

    private void send() {
        String text = payload.getText().toString();
        Log.i(TAG, "activity.send payload=" + text);
        Intent intent = new Intent(MainActivity.this, EchoService.class);
        intent.putExtra(EchoService.EXTRA_PAYLOAD, text);
        startService(intent);
    }

    @Override
    protected void onResume() {
        super.onResume();
        Log.i(TAG, "onResume");
        IntentFilter filter = new IntentFilter(EchoService.ACTION_ECHO);
        registerReceiver(uiReceiver, filter);
        registered = true;
    }

    @Override
    protected void onPause() {
        Log.i(TAG, "onPause");
        if (registered) {
            unregisterReceiver(uiReceiver);
            registered = false;
        }
        super.onPause();
    }

    @Override
    protected void onStart() {
        super.onStart();
        Log.i(TAG, "onStart");
    }

    @Override
    protected void onStop() {
        super.onStop();
        Log.i(TAG, "onStop");
    }

    @Override
    protected void onDestroy() {
        super.onDestroy();
        Log.i(TAG, "onDestroy");
    }
}
