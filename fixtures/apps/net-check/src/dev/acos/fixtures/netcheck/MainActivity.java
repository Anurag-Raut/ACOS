package dev.acos.fixtures.netcheck;

import android.app.Activity;
import android.net.ConnectivityManager;
import android.net.LinkProperties;
import android.net.Network;
import android.net.NetworkCapabilities;
import android.os.Bundle;
import android.util.Log;
import android.widget.TextView;

import java.io.InputStream;
import java.net.HttpURLConnection;
import java.net.URL;

/**
 * SPIKE-NET fixture (ADR-0030 gate, items 4/5/6): the smallest app that needs
 * the real network.
 *
 * Observable behaviour, all on one screen:
 *  - TextView id=title        constant "Net Check"
 *  - TextView id=url          the URL it fetches
 *  - TextView id=connectivity what ConnectivityManager reports (item 5)
 *  - TextView id=status       "HTTP <code>" on success, otherwise the exception
 *                             class name — never a fake success (item 6)
 *
 * The request is a real HTTPS GET of https://example.com through
 * HttpURLConnection on a background thread; DNS goes through bionic's
 * dnsproxyd proxy (item 4) and TLS through the image's CA store (item 6).
 *
 * With ACOS_SPIKE_NET unset the same binary shows the honest failure:
 * `UnknownHostException` (no dnsproxyd responder) or `SocketException`
 * (AF_INET denied by seccomp). Both are logged with tag "NetCheck".
 */
public class MainActivity extends Activity {
    private static final String TAG = "NetCheck";
    private static final String URL_TEXT = "https://example.com";

    private TextView status;
    private TextView connectivity;

    @Override
    protected void onCreate(Bundle savedInstanceState) {
        super.onCreate(savedInstanceState);
        Log.i(TAG, "onCreate");
        setContentView(R.layout.main);
        status = (TextView) findViewById(R.id.status);
        connectivity = (TextView) findViewById(R.id.connectivity);
        ((TextView) findViewById(R.id.url)).setText(URL_TEXT);
        String caps = describeNetwork();
        connectivity.setText(caps);
        Log.i(TAG, "connectivity " + caps);
        fetch();
    }

    /** What ConnectivityManager/NetworkCapabilities say (item 5). */
    private String describeNetwork() {
        try {
            ConnectivityManager cm =
                    (ConnectivityManager) getSystemService(CONNECTIVITY_SERVICE);
            if (cm == null) {
                return "cm=null";
            }
            Network active = cm.getActiveNetwork();
            if (active == null) {
                return "active=none";
            }
            NetworkCapabilities nc = cm.getNetworkCapabilities(active);
            StringBuilder sb = new StringBuilder("active=");
            sb.append(active);
            sb.append(" internet=").append(nc != null
                    && nc.hasCapability(NetworkCapabilities.NET_CAPABILITY_INTERNET));
            sb.append(" validated=").append(nc != null
                    && nc.hasCapability(NetworkCapabilities.NET_CAPABILITY_VALIDATED));
            LinkProperties lp = cm.getLinkProperties(active);
            sb.append(" iface=").append(lp == null ? "none" : lp.getInterfaceName());
            sb.append(" addrs=").append(lp == null ? 0 : lp.getLinkAddresses().size());
            sb.append(" dns=").append(lp == null ? 0 : lp.getDnsServers().size());
            sb.append(" routes=").append(lp == null ? 0 : lp.getRoutes().size());
            return sb.toString();
        } catch (Throwable t) {
            return "connectivity-exc=" + t.getClass().getName();
        }
    }

    /** The actual HTTPS GET (items 4 and 6). */
    private void fetch() {
        new Thread(new Runnable() {
            @Override
            public void run() {
                String text = "";
                try {
                    URL url = new URL(URL_TEXT);
                    HttpURLConnection c = (HttpURLConnection) url.openConnection();
                    c.setConnectTimeout(15000);
                    c.setReadTimeout(15000);
                    c.setInstanceFollowRedirects(false);
                    c.setRequestMethod("GET");
                    int code = c.getResponseCode();
                    text = "HTTP " + code;
                    try {
                        InputStream in = c.getInputStream();
                        byte[] buf = new byte[256];
                        int n = in.read(buf);
                        if (n > 0) {
                            Log.i(TAG, "body-first-bytes-len=" + n);
                        }
                        in.close();
                    } catch (Throwable ignored) {
                        // The status code is the assertion; the body is not.
                    }
                    c.disconnect();
                } catch (Throwable t) {
                    Log.w(TAG, "fetch failed: " + t.getClass().getName() + ": " + t.getMessage());
                    text = t.getClass().getName();
                }
                Log.i(TAG, "fetch result=" + text);
                final String shown = text;
                runOnUiThread(new Runnable() {
                    @Override
                    public void run() {
                        status.setText(shown);
                    }
                });
            }
        }, "net-check-fetch").start();
    }
}
