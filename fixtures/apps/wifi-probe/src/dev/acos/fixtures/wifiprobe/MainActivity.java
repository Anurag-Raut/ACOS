package dev.acos.fixtures.wifiprobe;

import android.app.Activity;
import android.content.BroadcastReceiver;
import android.content.Context;
import android.content.Intent;
import android.content.IntentFilter;
import android.net.ConnectivityManager;
import android.net.DhcpInfo;
import android.net.Network;
import android.net.NetworkCapabilities;
import android.net.NetworkInfo;
import android.net.wifi.ScanResult;
import android.net.wifi.WifiInfo;
import android.net.wifi.WifiManager;
import android.os.Bundle;
import android.os.Looper;
import android.util.Log;
import android.view.Gravity;
import android.view.View;
import android.widget.Button;
import android.widget.LinearLayout;
import android.widget.ScrollView;
import android.widget.TextView;

import java.util.List;
import java.util.concurrent.Executor;

/**
 * The `net.wifi` proof fixture (MODFILL).
 *
 * <p>Everything printed is a value the framework's own {@code WifiManager} /
 * {@code ConnectivityManager} returned or an event it delivered: the
 * {@code WifiInfo} parcel, the {@code ScanResult} list, the
 * {@code WIFI_STATE_CHANGED} / {@code STATE_CHANGE} / {@code RSSI_CHANGED} /
 * {@code SCAN_RESULTS} broadcasts, and the default-network
 * {@code NetworkCallback} (the module's connectivity consistency).
 * Lines are appended, never replaced.
 */
public final class MainActivity extends Activity {
    private static final String TAG = "wifiprobe";

    private TextView status;
    private TextView title;
    private WifiManager wifi;
    private ConnectivityManager connectivity;
    private boolean listening;

    @Override
    protected void onCreate(Bundle savedInstanceState) {
        super.onCreate(savedInstanceState);
        LinearLayout root = new LinearLayout(this);
        root.setOrientation(LinearLayout.VERTICAL);
        root.setPadding(24, 24, 24, 24);

        title = new TextView(this);
        title.setId(R.id.title);
        title.setTextSize(20);
        title.setText("WIFIPROBE");
        root.addView(title);

        wifi = (WifiManager) getApplicationContext().getSystemService(Context.WIFI_SERVICE);
        connectivity = getSystemService(ConnectivityManager.class);

        root.addView(button("Info", view -> info()));
        root.addView(button("Listen", view -> listen()));
        root.addView(button("Scan", view -> scan()));
        root.addView(button("Results", view -> results()));
        root.addView(button("Toggle", view -> toggle()));
        root.addView(button("Net", view -> net()));
        root.addView(button("Dhcp", view -> dhcp()));

        status = new TextView(this);
        status.setId(R.id.status);
        status.setTextIsSelectable(true);
        root.addView(status);
        ScrollView scroll = new ScrollView(this);
        scroll.addView(root);
        setContentView(scroll);
        log("READY wifi=" + (wifi == null ? "none" : "present")
                + " feature=" + getPackageManager().hasSystemFeature("android.hardware.wifi")
                + " service=" + serviceBinder());
    }

    /** Hidden API through reflection: is a `wifi` binder published? */
    private static String serviceBinder() {
        try {
            Object binder = Class.forName("android.os.ServiceManager")
                    .getMethod("checkService", String.class).invoke(null, "wifi");
            return binder == null ? "null" : "present";
        } catch (Throwable failure) {
            return "err:" + failure.getClass().getSimpleName();
        }
    }

    private Button button(String label, View.OnClickListener click) {
        Button button = new Button(this);
        button.setText(label);
        button.setGravity(Gravity.START);
        button.setOnClickListener(click);
        return button;
    }

    private void log(String line) {
        Log.i(TAG, line);
        Runnable append = () -> status.append(line + "\n");
        if (Looper.myLooper() == Looper.getMainLooper()) {
            append.run();
        } else {
            status.post(append);
        }
    }

    @SuppressWarnings("deprecation")
    private void info() {
        if (wifi == null) {
            log("INFO none");
            return;
        }
        try {
            WifiInfo info = wifi.getConnectionInfo();
            log("INFO enabled=" + wifi.isWifiEnabled() + " state=" + wifi.getWifiState()
                    + " ssid=" + info.getSSID() + " bssid=" + info.getBSSID()
                    + " rssi=" + info.getRssi() + " freq=" + info.getFrequency()
                    + " speed=" + info.getLinkSpeed() + " netid=" + info.getNetworkId()
                    + " supp=" + info.getSupplicantState()
                    + " level=" + wifi.calculateSignalLevel(info.getRssi())
                    + " 5ghz=" + wifi.is5GHzBandSupported());
        } catch (Throwable failure) {
            log("INFO threw=" + failure.getClass().getName() + ": " + failure.getMessage());
        }
    }

    private void listen() {
        if (listening) {
            log("LISTEN already");
            return;
        }
        listening = true;
        IntentFilter filter = new IntentFilter();
        filter.addAction(WifiManager.WIFI_STATE_CHANGED_ACTION);
        filter.addAction(WifiManager.NETWORK_STATE_CHANGED_ACTION);
        filter.addAction(WifiManager.RSSI_CHANGED_ACTION);
        filter.addAction(WifiManager.SCAN_RESULTS_AVAILABLE_ACTION);
        registerReceiver(new BroadcastReceiver() {
            @Override
            @SuppressWarnings("deprecation")
            public void onReceive(Context context, Intent intent) {
                String action = intent.getAction();
                if (WifiManager.WIFI_STATE_CHANGED_ACTION.equals(action)) {
                    log("BCAST wifi_state=" + intent.getIntExtra(WifiManager.EXTRA_WIFI_STATE, -1)
                            + " previous=" + intent.getIntExtra(
                                    WifiManager.EXTRA_PREVIOUS_WIFI_STATE, -1));
                } else if (WifiManager.NETWORK_STATE_CHANGED_ACTION.equals(action)) {
                    NetworkInfo net = intent.getParcelableExtra(WifiManager.EXTRA_NETWORK_INFO);
                    WifiInfo info = intent.getParcelableExtra(WifiManager.EXTRA_WIFI_INFO);
                    log("BCAST network=" + (net == null ? "null" : net.getState())
                            + " ssid=" + (info == null ? "null" : info.getSSID())
                            + " bssid=" + intent.getStringExtra(WifiManager.EXTRA_BSSID));
                } else if (WifiManager.RSSI_CHANGED_ACTION.equals(action)) {
                    log("BCAST rssi=" + intent.getIntExtra(WifiManager.EXTRA_NEW_RSSI, 0));
                } else if (WifiManager.SCAN_RESULTS_AVAILABLE_ACTION.equals(action)) {
                    log("BCAST scan_results updated="
                            + intent.getBooleanExtra(WifiManager.EXTRA_RESULTS_UPDATED, false));
                }
            }
        }, filter);
        connectivity.registerDefaultNetworkCallback(new ConnectivityManager.NetworkCallback() {
            @Override
            public void onAvailable(Network network) {
                log("NETCB available");
            }

            @Override
            public void onLost(Network network) {
                log("NETCB lost");
            }

            @Override
            public void onCapabilitiesChanged(Network network, NetworkCapabilities caps) {
                log("NETCB caps wifi=" + caps.hasTransport(NetworkCapabilities.TRANSPORT_WIFI)
                        + " validated="
                        + caps.hasCapability(NetworkCapabilities.NET_CAPABILITY_VALIDATED));
            }
        });
        if (wifi != null) {
            wifi.registerScanResultsCallback(new Executor() {
                @Override
                public void execute(Runnable command) {
                    command.run();
                }
            }, new WifiManager.ScanResultsCallback() {
                @Override
                public void onScanResultsAvailable() {
                    log("SCANCB available");
                }
            });
        }
        log("LISTEN registered");
    }

    @SuppressWarnings("deprecation")
    private void scan() {
        try {
            log("SCAN started=" + wifi.startScan());
        } catch (Throwable failure) {
            log("SCAN threw=" + failure.getClass().getName() + ": " + failure.getMessage());
        }
    }

    private void results() {
        try {
            List<ScanResult> list = wifi.getScanResults();
            StringBuilder out = new StringBuilder("RESULTS n=" + list.size());
            for (ScanResult result : list) {
                out.append(" [").append(result.SSID).append(' ').append(result.BSSID)
                        .append(' ').append(result.level).append(' ').append(result.frequency)
                        .append(' ').append(result.capabilities).append(']');
            }
            log(out.toString());
        } catch (Throwable failure) {
            log("RESULTS threw=" + failure.getClass().getName() + ": " + failure.getMessage());
        }
    }

    @SuppressWarnings("deprecation")
    private void toggle() {
        try {
            boolean accepted = wifi.setWifiEnabled(false);
            log("TOGGLE accepted=" + accepted + " enabled=" + wifi.isWifiEnabled());
        } catch (Throwable failure) {
            log("TOGGLE threw=" + failure.getClass().getName() + ": " + failure.getMessage());
        }
    }

    private void net() {
        try {
            Network active = connectivity.getActiveNetwork();
            NetworkCapabilities caps = active == null ? null
                    : connectivity.getNetworkCapabilities(active);
            log("NET active=" + (active != null)
                    + " wifi=" + (caps != null && caps.hasTransport(
                            NetworkCapabilities.TRANSPORT_WIFI))
                    + " validated=" + (caps != null && caps.hasCapability(
                            NetworkCapabilities.NET_CAPABILITY_VALIDATED)));
        } catch (Throwable failure) {
            log("NET threw=" + failure.getClass().getName() + ": " + failure.getMessage());
        }
    }

    @SuppressWarnings("deprecation")
    private void dhcp() {
        try {
            DhcpInfo info = wifi.getDhcpInfo();
            log("DHCP ip=" + info.ipAddress + " gateway=" + info.gateway
                    + " dns1=" + info.dns1 + " lease=" + info.leaseDuration);
        } catch (Throwable failure) {
            log("DHCP threw=" + failure.getClass().getName() + ": " + failure.getMessage());
        }
    }
}
