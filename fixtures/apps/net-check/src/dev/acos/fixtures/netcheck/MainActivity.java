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
        String dns = probeDnsProxy();
        ((TextView) findViewById(R.id.dnsprobe)).setText(dns);
        Log.i(TAG, "dnsprobe " + dns);
        fetch();
    }

    /**
     * Item 4 diagnostics: can the app process see and talk to the netd resolver
     * proxy at all? Two facts, each with its failure spelled out:
     *  - the library bionic's `netdClientInit()` dlopens to install
     *    `dnsOpenProxy` (without it, every `getaddrinfo` answers EAI_NODATA and
     *    the request never reaches the socket);
     *  - a raw `getaddrinfo` request on /dev/socket/dnsproxyd, byte for byte what
     *    `android_getaddrinfo_proxy()` writes, and the reply's first bytes.
     * It never substitutes for the real fetch: `id=status` still comes from
     * HttpURLConnection.
     */
    /** First `n` characters of a diagnostic, so one long message cannot fill the screen. */
    private static String shortText(String s, int n) {
        String one = s.replace('\n', ' ');
        return one.length() <= n ? one : one.substring(0, n) + "...";
    }

    private String probeDnsProxy() {
        StringBuilder sb = new StringBuilder("dnsprobe");
        try {
            java.io.File f = new java.io.File("/dev/socket/dnsproxyd");
            sb.append(" exists=").append(f.exists());
            sb.append(" canRead=").append(f.canRead());
            sb.append(" canWrite=").append(f.canWrite());
        } catch (Throwable t) {
            sb.append(" stat-exc=").append(t.getClass().getName());
        }
        try {
            System.loadLibrary("netd_client");
            sb.append(" dlopen=ok");
        } catch (Throwable t) {
            sb.append(" dlopen=").append(t.getClass().getSimpleName()).append(':')
                    .append(shortText(String.valueOf(t.getMessage()), 70));
        }
        android.net.LocalSocket s = new android.net.LocalSocket(
                android.net.LocalSocket.SOCKET_STREAM);
        try {
            s.connect(new android.net.LocalSocketAddress("/dev/socket/dnsproxyd",
                    android.net.LocalSocketAddress.Namespace.FILESYSTEM));
            java.io.OutputStream out = s.getOutputStream();
            out.write("getaddrinfo example.com ^ 0 0 0 0 0".getBytes("US-ASCII"));
            out.write(0);
            out.flush();
            byte[] head = new byte[4];
            int n = s.getInputStream().read(head);
            sb.append(" proxy=bytes:").append(n);
            if (n > 0) {
                sb.append(" code=").append(new String(head, 0, Math.min(n, 3), "US-ASCII"));
            }
        } catch (Throwable t) {
            sb.append(" proxy=").append(t.getClass().getSimpleName()).append(':')
                    .append(shortText(String.valueOf(t.getMessage()), 70));
        } finally {
            try {
                s.close();
            } catch (Throwable ignored) {
            }
        }
        return sb.toString();
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
            sb.append(" dnsList=");
            if (lp == null) {
                sb.append("none");
            } else {
                StringBuilder dns = new StringBuilder();
                for (java.net.InetAddress a : lp.getDnsServers()) {
                    if (dns.length() > 0) {
                        dns.append(',');
                    }
                    dns.append(a.getHostAddress());
                }
                sb.append(dns.length() == 0 ? "none" : dns.toString());
            }
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
