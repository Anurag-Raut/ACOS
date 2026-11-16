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

    /**
     * NET6: a fetch that has been pending this long is a bug, not a slow
     * network, and "pending forever" is not an answer. The watchdog logs every
     * thread's Java stack (the same frames ART's SIGQUIT dump prints) once per
     * fetch, so a stuck fetch says where it is stuck instead of silently
     * hanging. It never changes what the fetch itself does.
     */
    private static final long STUCK_AFTER_MS = 8000;
    private volatile long fetchStartedAt;
    private volatile boolean fetchRunning;
    private final java.util.concurrent.atomic.AtomicBoolean fetchDumped =
            new java.util.concurrent.atomic.AtomicBoolean();

    private void startWatchdog() {
        new Thread(new Runnable() {
            @Override
            public void run() {
                while (true) {
                    try {
                        Thread.sleep(1000);
                    } catch (InterruptedException e) {
                        return;
                    }
                    if (!fetchRunning) {
                        continue;
                    }
                    long pending = android.os.SystemClock.uptimeMillis() - fetchStartedAt;
                    if (pending < STUCK_AFTER_MS || !fetchDumped.compareAndSet(false, true)) {
                        continue;
                    }
                    Log.w(TAG, "stuck fetch: pending_ms=" + pending
                            + " — dumping all thread stacks");
                    for (java.util.Map.Entry<Thread, StackTraceElement[]> e
                            : Thread.getAllStackTraces().entrySet()) {
                        Thread t = e.getKey();
                        StringBuilder sb = new StringBuilder("STACK thread=");
                        sb.append(t.getName()).append(" state=").append(t.getState());
                        Log.w(TAG, sb.toString());
                        for (StackTraceElement f : e.getValue()) {
                            Log.w(TAG, "STACK   at " + f);
                        }
                    }
                    Log.w(TAG, "stuck fetch: dump done (pending_ms=" + pending + ")");
                }
            }
        }, "net-check-watchdog").start();
    }

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
        startWatchdog();
        String dns = probeDnsProxy();
        ((TextView) findViewById(R.id.dnsprobe)).setText(dns);
        Log.i(TAG, "dnsprobe " + dns);
        // NET5: `retry` re-runs the same real fetch in the SAME guest process, so
        // a mid-session control (the network module's `offline`/`online`) can be
        // observed the way an app observes it: the status line changes to the
        // exception class and comes back to `HTTP 200`. Nothing else about the
        // screen changes; the URL is the same.
        findViewById(R.id.retry).setOnClickListener(new android.view.View.OnClickListener() {
            @Override
            public void onClick(android.view.View view) {
                status.setText(R.string.pending);
                Log.i(TAG, "retry tapped");
                fetch();
            }
        });
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
        fetchDumped.set(false);
        fetchStartedAt = android.os.SystemClock.uptimeMillis();
        fetchRunning = true;
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
                    Log.i(TAG, "response code=" + code + " url=" + c.getURL()
                            + " server=" + c.getHeaderField("Server")
                            + " cfray=" + c.getHeaderField("cf-ray")
                            + " via=" + c.getHeaderField("Via"));
                    try {
                        InputStream err = c.getErrorStream();
                        if (err != null) {
                            byte[] buf = new byte[200];
                            int n = err.read(buf);
                            Log.i(TAG, "response body=" + (n > 0
                                    ? new String(buf, 0, n, "UTF-8").replace('\n', ' ')
                                    : ""));
                            err.close();
                        }
                    } catch (Throwable ignored) {
                    }
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
                fetchRunning = false;
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
