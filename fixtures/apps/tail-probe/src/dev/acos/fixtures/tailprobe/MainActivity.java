package dev.acos.fixtures.tailprobe;

import android.app.Activity;
import android.bluetooth.BluetoothAdapter;
import android.bluetooth.BluetoothDevice;
import android.bluetooth.BluetoothManager;
import android.content.BroadcastReceiver;
import android.content.Context;
import android.content.Intent;
import android.content.IntentFilter;
import android.hardware.biometrics.BiometricManager;
import android.hardware.biometrics.BiometricPrompt;
import android.os.Bundle;
import android.os.CancellationSignal;
import android.provider.Telephony;
import android.telephony.PhoneStateListener;
import android.telephony.SmsManager;
import android.telephony.SmsMessage;
import android.telephony.TelephonyManager;
import android.os.VibrationEffect;
import android.os.Vibrator;
import android.view.Gravity;
import android.view.View;
import android.widget.Button;
import android.widget.LinearLayout;
import android.widget.TextView;

import java.util.Set;

/**
 * The long-tail module fixture: one tab per module, each reporting exactly what
 * the framework's own client classes answered.
 *
 * <p>The app never claims a module is attached: every line it prints is a value
 * it read through a real API (`TelephonyManager`, `SmsMessage.createFromPdu`,
 * `PhoneStateListener`, `BiometricManager`, `BiometricPrompt`,
 * `BluetoothAdapter`, `Vibrator`) or an event it received. With a module
 * detached the same code prints the honest missing path (`NO_BLUETOOTH`,
 * `SIM_STATE_UNKNOWN`, ...).
 *
 * <p>Lines are appended, never replaced, so a scenario's `wait_for` can wait for
 * a specific line after it drives a control op.
 */
public final class MainActivity extends Activity {
    private LinearLayout root;
    private TextView status;
    private TextView title;
    private TelephonyManager telephony;
    private BluetoothAdapter adapter;
    private int callStateLines;

    @Override
    protected void onCreate(Bundle savedInstanceState) {
        super.onCreate(savedInstanceState);

        root = new LinearLayout(this);
        root.setOrientation(LinearLayout.VERTICAL);
        root.setPadding(24, 24, 24, 24);

        title = new TextView(this);
        title.setId(R.id.title);
        title.setTextSize(20);
        title.setText("TAILPROBE");
        root.addView(title);

        root.addView(tab("Telephony", "telephony-tab", view -> showTelephony()));
        root.addView(tab("Biometric", "biometric-tab", view -> showBiometric()));
        root.addView(tab("Bluetooth", "bluetooth-tab", view -> showBluetooth()));
        root.addView(tab("Vibrator", "vibrator-tab", view -> showVibrator()));

        status = new TextView(this);
        status.setId(R.id.status);
        status.setTextIsSelectable(true);
        root.addView(status);

        setContentView(root);

        telephony = getSystemService(TelephonyManager.class);
        registerSmsReceiver();
        registerCallStateListener();
        registerBluetoothReceivers();

        log("READY");
        showTelephony();
    }

    private Button tab(String label, String tag, View.OnClickListener click) {
        Button button = new Button(this);
        button.setTag(tag);
        button.setText(label);
        button.setGravity(Gravity.START);
        button.setOnClickListener(click);
        return button;
    }

    private void log(String line) {
        status.append(line + "\n");
    }

    // ------------------------------------------------------------ telephony

    private void showTelephony() {
        title.setText("TELEPHONY");
        if (telephony == null) {
            log("TELEPHONY none (no TelephonyManager)");
            return;
        }
        int sim;
        int phone;
        int network;
        int call;
        try {
            sim = telephony.getSimState();
            phone = telephony.getPhoneType();
            network = telephony.getNetworkType();
            call = telephony.getCallStateForSubscription();
        } catch (Throwable missing) {
            // A phone process installs `TelephonyServiceManager` before any app
            // runs; a session with no telephony module has none, so the
            // framework's own client NPEs here. That is the honest missing path
            // (not a fake SIM_STATE_UNKNOWN).
            log("TELEPHONY unavailable=" + missing.getClass().getSimpleName());
            return;
        }
        String line1 = safe(() -> telephony.getLine1Number());
        String operator = safe(() -> telephony.getSimOperator());
        log("TELEPHONY sim=" + simName(sim)
                + " phone=" + phoneName(phone)
                + " network=" + networkName(network)
                + " call=" + callName(call)
                + " line1=" + (line1 == null ? "null" : line1)
                + " operator=" + (operator == null ? "null" : ("\"" + operator + "\"")));

        Button listen = new Button(this);
        listen.setText("Listen for calls");
        listen.setTag("listen-calls");
        listen.setOnClickListener(view -> registerCallStateListener());
        appendToRoot(listen);

        Button send = new Button(this);
        send.setText("Send SMS");
        send.setTag("send-sms");
        send.setOnClickListener(view -> {
            try {
                SmsManager sms = getSystemService(SmsManager.class);
                if (sms == null) {
                    log("SMS-SENT none (no SmsManager: FEATURE_TELEPHONY_MESSAGING absent)");
                    return;
                }
                sms.sendTextMessage("+15557654321", null, "hello from app", null, null);
                log("SMS-SENT queued to=+15557654321");
            } catch (Throwable error) {
                log("SMS-SENT error=" + error.getClass().getSimpleName()
                        + " msg=" + error.getMessage());
            }
        });
        appendToRoot(send);
    }

    private void registerSmsReceiver() {
        BroadcastReceiver receiver = new BroadcastReceiver() {
            @Override
            public void onReceive(Context context, Intent intent) {
                SmsMessage[] messages = Telephony.Sms.Intents.getMessagesFromIntent(intent);
                if (messages == null) {
                    log("SMS undecodable");
                    return;
                }
                for (SmsMessage message : messages) {
                    log("SMS from=" + message.getOriginatingAddress()
                            + " body=" + message.getMessageBody());
                }
            }
        };
        registerReceiver(receiver, new IntentFilter(Telephony.Sms.Intents.SMS_RECEIVED_ACTION));
    }

    private PhoneStateListener callListener;

    private void registerCallStateListener() {
        if (telephony == null) {
            return;
        }
        PhoneStateListener listener = new PhoneStateListener() {
            @Override
            public void onCallStateChanged(int state, String phoneNumber) {
                callStateLines++;
                log("CALL state=" + callName(state)
                        + " number=" + (phoneNumber == null ? "null" : phoneNumber));
            }
        };
        callListener = listener;
        try {
            telephony.listen(listener, PhoneStateListener.LISTEN_CALL_STATE);
        } catch (Throwable missing) {
            log("CALL unavailable=" + missing.getClass().getSimpleName());
        }
    }

    // ------------------------------------------------------------ biometric

    private void showBiometric() {
        title.setText("BIOMETRIC");
        BiometricManager manager = getSystemService(BiometricManager.class);
        if (manager == null) {
            log("BIOMETRIC none (no BiometricManager)");
            return;
        }
        final int allowed = BiometricManager.Authenticators.BIOMETRIC_STRONG;
        try {
            log("BIOMETRIC canAuthenticate=" + canName(manager.canAuthenticate(allowed)));
        } catch (Throwable missing) {
            // A `hw.biometric`-less session answers the `auth` tombstone with a
            // structured UnsupportedOperationException, which the client does not
            // catch (it is not a RemoteException). That is the honest missing
            // path; the probe reports it instead of crashing.
            log("BIOMETRIC unavailable=" + missing.getClass().getSimpleName());
        }

        Button auth = new Button(this);
        auth.setText("Authenticate");
        auth.setTag("authenticate");
        auth.setOnClickListener(view -> startPrompt());
        appendToRoot(auth);
    }

    private void startPrompt() {
        try {
            BiometricPrompt prompt = new BiometricPrompt.Builder(this)
                    .setTitle("ACOS biometric")
                    .setNegativeButton("Cancel", getMainExecutor(),
                            (dialog, which) -> log("AUTH:CANCELED-BUTTON"))
                    .build();
            prompt.authenticate(new CancellationSignal(), getMainExecutor(),
                    new BiometricPrompt.AuthenticationCallback() {
                        @Override
                        public void onAuthenticationSucceeded(
                                BiometricPrompt.AuthenticationResult result) {
                            log("AUTH:SUCCESS type=" + result.getAuthenticationType());
                        }

                        @Override
                        public void onAuthenticationFailed() {
                            log("AUTH:FAILED");
                        }

                        @Override
                        public void onAuthenticationError(int errorCode, CharSequence message) {
                            log("AUTH:ERROR code=" + errorCode + " msg=" + message);
                        }
                    });
            log("AUTH:REQUESTED");
        } catch (Throwable error) {
            log("AUTH:REQUEST-FAILED " + error.getClass().getSimpleName());
        }
    }

    // ------------------------------------------------------------ bluetooth

    private void showBluetooth() {
        title.setText("BLUETOOTH");
        BluetoothManager manager = getSystemService(BluetoothManager.class);
        final BluetoothAdapter current = manager == null ? null : manager.getAdapter();
        adapter = current;
        if (adapter == null) {
            log("BLUETOOTH none (no adapter)");
            return;
        }
        log("BLUETOOTH state=" + stateName(adapter.getState())
                + " name=" + safe(adapter::getName)
                + " address=" + safe(adapter::getAddress));
        // `getBondedDevices` is a oneway call that waits on the binder result
        // receiver, so a slow answer must never block the UI thread.
        final BluetoothAdapter bondedAdapter = adapter;
        new Thread(() -> {
            try {
                Set<BluetoothDevice> bonded = bondedAdapter.getBondedDevices();
                if (bonded == null) {
                    runOnUiThread(() -> log("BONDED null"));
                    return;
                }
                final int count = bonded.size();
                final StringBuilder names = new StringBuilder();
                for (BluetoothDevice device : bonded) {
                    names.append("BONDED device=").append(device.getName())
                            .append(' ').append(device.getAddress()).append('\n');
                }
                runOnUiThread(() -> {
                    log("BONDED count=" + count);
                    if (names.length() > 0) {
                        status.append(names);
                    }
                });
            } catch (Throwable error) {
                runOnUiThread(() -> log("BONDED error=" + error.getClass().getSimpleName()));
            }
        }, "bonded").start();

        Button scan = new Button(this);
        scan.setText("Scan");
        scan.setTag("scan");
        scan.setOnClickListener(view -> {
            final BluetoothAdapter scanAdapter = adapter;
            new Thread(() -> {
                try {
                    boolean started = scanAdapter.startDiscovery();
                    runOnUiThread(() -> log("SCAN started=" + started));
                } catch (Throwable error) {
                    runOnUiThread(() -> log("SCAN error=" + error.getClass().getSimpleName()));
                }
            }, "scan").start();
        });
        appendToRoot(scan);
    }

    private void registerBluetoothReceivers() {
        registerReceiver(new BroadcastReceiver() {
            @Override
            public void onReceive(Context context, Intent intent) {
                BluetoothDevice device = intent.getParcelableExtra(BluetoothDevice.EXTRA_DEVICE);
                log("FOUND name=" + (device == null ? "null" : device.getName())
                        + " address=" + (device == null ? "null" : device.getAddress())
                        + " rssi=" + intent.getShortExtra(BluetoothDevice.EXTRA_RSSI, (short) 0));
            }
        }, new IntentFilter(BluetoothDevice.ACTION_FOUND));
        registerReceiver(new BroadcastReceiver() {
            @Override
            public void onReceive(Context context, Intent intent) {
                log("BTSTATE " + stateName(
                        intent.getIntExtra(BluetoothAdapter.EXTRA_STATE, -1)));
            }
        }, new IntentFilter(BluetoothAdapter.ACTION_STATE_CHANGED));
    }

    // ------------------------------------------------------------- vibrator

    private void showVibrator() {
        title.setText("VIBRATOR");
        Vibrator vibrator = getSystemService(Vibrator.class);
        if (vibrator == null) {
            log("VIBRATOR none (no Vibrator)");
            return;
        }
        try {
            log("VIBRATOR hasVibrator=" + vibrator.hasVibrator());
        } catch (Throwable missing) {
            // The `vibrator_manager` tombstone answers a structured
            // UnsupportedOperationException; a probe reports it, never crashes.
            log("VIBRATOR unavailable=" + missing.getClass().getSimpleName());
        }
        Button one = new Button(this);
        one.setText("Vibrate");
        one.setTag("vibrate");
        one.setOnClickListener(view -> {
            try {
                vibrator.vibrate(VibrationEffect.createOneShot(250, 128));
                log("VIBRATED one-shot-250-128");
            } catch (Throwable error) {
                log("VIBRATED error=" + error.getClass().getSimpleName());
            }
        });
        appendToRoot(one);
        Button two = new Button(this);
        two.setText("Vibrate2");
        two.setTag("vibrate2");
        two.setOnClickListener(view -> {
            try {
                vibrator.vibrate(VibrationEffect.createOneShot(250, 255));
                log("VIBRATED one-shot-250-255");
            } catch (Throwable error) {
                log("VIBRATED error=" + error.getClass().getSimpleName());
            }
        });
        appendToRoot(two);
    }

    // --------------------------------------------------------------- helpers

    private void appendToRoot(View view) {
        // Keep the status text last so the log stays below the controls.
        int index = root.indexOfChild(status);
        root.addView(view, index);
    }

    private interface Supplier {
        String get() throws Throwable;
    }

    private static String safe(Supplier supplier) {
        try {
            return supplier.get();
        } catch (Throwable error) {
            return "error:" + error.getClass().getSimpleName();
        }
    }

    private static String simName(int state) {
        switch (state) {
            case TelephonyManager.SIM_STATE_ABSENT: return "ABSENT";
            case TelephonyManager.SIM_STATE_PIN_REQUIRED: return "PIN_REQUIRED";
            case TelephonyManager.SIM_STATE_PUK_REQUIRED: return "PUK_REQUIRED";
            case TelephonyManager.SIM_STATE_NETWORK_LOCKED: return "NETWORK_LOCKED";
            case TelephonyManager.SIM_STATE_READY: return "READY";
            case TelephonyManager.SIM_STATE_NOT_READY: return "NOT_READY";
            case TelephonyManager.SIM_STATE_PERM_DISABLED: return "PERM_DISABLED";
            case TelephonyManager.SIM_STATE_CARD_IO_ERROR: return "CARD_IO_ERROR";
            case TelephonyManager.SIM_STATE_CARD_RESTRICTED: return "CARD_RESTRICTED";
            case TelephonyManager.SIM_STATE_UNKNOWN: return "UNKNOWN";
            default: return "OTHER(" + state + ")";
        }
    }

    private static String phoneName(int type) {
        switch (type) {
            case TelephonyManager.PHONE_TYPE_NONE: return "NONE";
            case TelephonyManager.PHONE_TYPE_GSM: return "GSM";
            case TelephonyManager.PHONE_TYPE_CDMA: return "CDMA";
            case TelephonyManager.PHONE_TYPE_SIP: return "SIP";
            default: return "OTHER(" + type + ")";
        }
    }

    private static String networkName(int type) {
        switch (type) {
            case TelephonyManager.NETWORK_TYPE_GPRS: return "GPRS";
            case TelephonyManager.NETWORK_TYPE_EDGE: return "EDGE";
            case TelephonyManager.NETWORK_TYPE_UMTS: return "UMTS";
            case TelephonyManager.NETWORK_TYPE_HSDPA: return "HSDPA";
            case TelephonyManager.NETWORK_TYPE_HSPA: return "HSPA";
            case TelephonyManager.NETWORK_TYPE_LTE: return "LTE";
            case TelephonyManager.NETWORK_TYPE_NR: return "NR";
            case TelephonyManager.NETWORK_TYPE_IWLAN: return "IWLAN";
            case TelephonyManager.NETWORK_TYPE_UNKNOWN: return "UNKNOWN";
            default: return "OTHER(" + type + ")";
        }
    }

    private static String callName(int state) {
        switch (state) {
            case TelephonyManager.CALL_STATE_IDLE: return "IDLE";
            case TelephonyManager.CALL_STATE_RINGING: return "RINGING";
            case TelephonyManager.CALL_STATE_OFFHOOK: return "OFFHOOK";
            default: return "OTHER(" + state + ")";
        }
    }

    private static String canName(int result) {
        switch (result) {
            case BiometricManager.BIOMETRIC_SUCCESS: return "SUCCESS";
            case BiometricManager.BIOMETRIC_ERROR_NO_HARDWARE: return "NO_HARDWARE";
            case BiometricManager.BIOMETRIC_ERROR_HW_UNAVAILABLE: return "HW_UNAVAILABLE";
            case BiometricManager.BIOMETRIC_ERROR_NONE_ENROLLED: return "NONE_ENROLLED";
            case BiometricManager.BIOMETRIC_ERROR_SECURITY_UPDATE_REQUIRED:
                return "SECURITY_UPDATE_REQUIRED";
            default: return "OTHER(" + result + ")";
        }
    }

    private static String stateName(int state) {
        switch (state) {
            case BluetoothAdapter.STATE_OFF: return "OFF";
            case BluetoothAdapter.STATE_TURNING_ON: return "TURNING_ON";
            case BluetoothAdapter.STATE_ON: return "ON";
            case BluetoothAdapter.STATE_TURNING_OFF: return "TURNING_OFF";
            default: return "OTHER(" + state + ")";
        }
    }
}
