package dev.acos.fixtures.pmsparcel;

import android.app.Activity;
import android.content.Context;
import android.content.pm.PackageInfo;
import android.content.pm.PackageManager;
import android.net.ConnectivityManager;
import android.net.LinkProperties;
import android.net.Network;
import android.net.NetworkCapabilities;
import android.os.Bundle;
import android.util.Log;
import android.widget.TextView;

/**
 * REL3P fixture: the reply of
 * {@code getPackageInfo(own package, GET_SIGNATURES|GET_META_DATA|GET_PERMISSIONS)}
 * -- {@code 0x100024c0}, the flags {@code WebViewFactory} uses -- must decode in
 * the framework's own {@code PackageInfo.CREATOR} without an exception.
 *
 * <p>{@code PackageInfo.writeToParcel} writes {@code splitNames} and
 * {@code requestedPermissions} with {@code writeString8Array} (length in UTF-8
 * bytes). Before REL3P ACOS wrote them as String16 arrays, so every field after
 * {@code requestedPermissions} was read from the middle of the string payload:
 * the framework built a {@code Signature[]} of 7 209 071 entries and threw
 * {@code OutOfMemoryError: Failed to allocate a 7676737936 byte allocation at
 * Parcel.createString8Array}. Any app calling this method with those flags hit
 * it, so the check is an app-visible one, not a unit test.
 *
 * <p>Observable behaviour:
 * <ul>
 *   <li>{@code TextView id=report} = {@code report sigs=N perms=N splits=N
 *       meta=Y/N pkg=<name>} after the call, or {@code FAIL <exception>} when
 *       the reply cannot be decoded;</li>
 *   <li>log tag {@code AcosPmsParcel}: {@code getPackageInfo flags=0x100024c0
 *       sigs=N perms=N splits=N meta=Y/N} followed by {@code OK}.</li>
 * </ul>
 *
 * <p>REL3P2 extends the fixture to the two {@code android.net} replies ACOS
 * serves today: {@code getNetworkCapabilities(getActiveNetwork())} and
 * {@code getLinkProperties(getActiveNetwork())}. Their specs are written with
 * the framework's real field ops now (`writeParcelable`, a counted
 * {@code ArraySet}, {@code writeTypedList}, the inlined {@code writeAddresses}
 * bodies), so a wrong field list shows up as a framework reader exception or a
 * wrong field here:
 * <ul>
 *   <li>{@code TextView id=net_report} = {@code net wifi=1 validated=0
 *       iface=wlan0 mtu=1500 dns=0 routes=0 proxy=null nat64=null
 *       stacked=0}, or {@code net FAIL <exception>};</li>
 *   <li>log tag {@code AcosPmsParcel}: {@code netcaps ok ...},
 *       {@code netlink ok ...}.</li>
 * </ul>
 */
public class MainActivity extends Activity {
    private static final String TAG = "AcosPmsParcel";

    /** GET_SIGNATURES|GET_META_DATA|GET_PERMISSIONS, as WebViewFactory asks. */
    private static final int FLAGS = 0x100024c0;

    @Override
    protected void onCreate(Bundle savedInstanceState) {
        super.onCreate(savedInstanceState);
        setContentView(R.layout.main);
        TextView report = (TextView) findViewById(R.id.report);
        Log.i(TAG, "onCreate");
        PackageManager pm = getPackageManager();
        try {
            PackageInfo pi = pm.getPackageInfo(getPackageName(), FLAGS);
            int sigs = pi.signatures == null ? -1 : pi.signatures.length;
            int perms =
                    pi.requestedPermissions == null ? -1 : pi.requestedPermissions.length;
            int splits = pi.splitNames == null ? -1 : pi.splitNames.length;
            boolean meta = pi.applicationInfo != null && pi.applicationInfo.metaData != null;
            // Touch the fields that follow the String8 arrays in
            // `PackageInfo.writeToParcel`: a desynchronised reader lands in the
            // middle of a string here instead of on the next field.
            int appFlags = pi.applicationInfo == null ? 0 : pi.applicationInfo.flags;
            String appClass =
                    pi.applicationInfo == null
                            ? "null"
                            : String.valueOf(pi.applicationInfo.className);
            Log.i(
                    TAG,
                    "getPackageInfo flags=0x"
                            + Integer.toHexString(FLAGS)
                            + " sigs="
                            + sigs
                            + " perms="
                            + perms
                            + " splits="
                            + splits
                            + " meta="
                            + (meta ? "Y" : "N")
                            + " appFlags="
                            + appFlags
                            + " appClass="
                            + appClass);
            report.setText(
                    "report sigs="
                            + sigs
                            + " perms="
                            + perms
                            + " splits="
                            + splits
                            + " meta="
                            + (meta ? "Y" : "N")
                            + " pkg="
                            + pi.packageName);
            Log.i(TAG, "OK");
        } catch (Throwable t) {
            // The pre-REL3P failure is an OutOfMemoryError inside the parcel
            // reader, so catch Throwable, not just Exception: the point of the
            // fixture is to surface it on screen instead of crashing.
            Log.e(TAG, "FAIL " + t, t);
            report.setText("FAIL " + t);
        }
        checkConnectivity();
    }

    /**
     * REL3P2: {@code getNetworkCapabilities} / {@code getLinkProperties} of the
     * active network must decode in the framework's own readers and carry the
     * documented offline values. The checks below read the fields ACOS writes
     * in `NetworkCapabilities.writeToParcel` / `LinkProperties.writeToParcel`
     * order; a desynchronised reply either throws inside the reader or lands on
     * a different value here.
     */
    private void checkConnectivity() {
        TextView net = (TextView) findViewById(R.id.net_report);
        try {
            ConnectivityManager cm =
                    (ConnectivityManager) getSystemService(Context.CONNECTIVITY_SERVICE);
            Network active = cm.getActiveNetwork();
            if (active == null) {
                throw new IllegalStateException("getActiveNetwork() is null");
            }
            NetworkCapabilities nc = cm.getNetworkCapabilities(active);
            if (nc == null) {
                throw new IllegalStateException("getNetworkCapabilities() is null");
            }
            boolean wifi = nc.hasTransport(NetworkCapabilities.TRANSPORT_WIFI);
            boolean validated = nc.hasCapability(NetworkCapabilities.NET_CAPABILITY_VALIDATED);
            boolean internet = nc.hasCapability(NetworkCapabilities.NET_CAPABILITY_INTERNET);
            int up = nc.getLinkUpstreamBandwidthKbps();
            int down = nc.getLinkDownstreamBandwidthKbps();
            int signal = nc.getSignalStrength();
            int owner = nc.getOwnerUid();
            boolean spec = nc.getNetworkSpecifier() != null;
            boolean tinfo = nc.getTransportInfo() != null;
            // The last field of the writer: a desync anywhere before it lands
            // on a wrong enterprise id or throws here.
            int enterprise = nc.getEnterpriseIds().length;
            Log.i(
                    TAG,
                    "netcaps ok wifi="
                            + (wifi ? 1 : 0)
                            + " internet="
                            + (internet ? 1 : 0)
                            + " validated="
                            + (validated ? 1 : 0)
                            + " up="
                            + up
                            + " down="
                            + down
                            + " signal="
                            + signal
                            + " owner="
                            + owner
                            + " spec="
                            + (spec ? "set" : "null")
                            + " tinfo="
                            + (tinfo ? "set" : "null")
                            + " enterprise="
                            + enterprise);
            if (!wifi || validated || !internet || up != 0 || down != 0 || signal != Integer.MIN_VALUE
                    || owner != 1000 || spec || tinfo || enterprise != 0) {
                throw new IllegalStateException(
                        "unexpected capabilities wifi="
                                + wifi
                                + " internet="
                                + internet
                                + " validated="
                                + validated
                                + " up="
                                + up
                                + " down="
                                + down
                                + " signal="
                                + signal
                                + " owner="
                                + owner
                                + " spec="
                                + spec
                                + " tinfo="
                                + tinfo
                                + " enterprise="
                                + enterprise);
            }

            LinkProperties lp = cm.getLinkProperties(active);
            if (lp == null) {
                throw new IllegalStateException("getLinkProperties() is null");
            }
            String iface = String.valueOf(lp.getInterfaceName());
            int mtu = lp.getMtu();
            int dns = lp.getDnsServers().size();
            int links = lp.getLinkAddresses().size();
            int routes = lp.getRoutes().size();
            boolean proxy = lp.getHttpProxy() != null;
            boolean nat64 = lp.getNat64Prefix() != null;
            // Fields after the address/route lists: a desync in the counted
            // `writeAddresses` helpers lands here.
            String domains = String.valueOf(lp.getDomains());
            String pdns = String.valueOf(lp.getPrivateDnsServerName());
            boolean wol = lp.isWakeOnLanSupported();
            Log.i(
                    TAG,
                    "netlink ok iface="
                            + iface
                            + " mtu="
                            + mtu
                            + " dns="
                            + dns
                            + " links="
                            + links
                            + " routes="
                            + routes
                            + " proxy="
                            + (proxy ? "set" : "null")
                            + " nat64="
                            + (nat64 ? "set" : "null")
                            + " domains="
                            + domains
                            + " pdns="
                            + pdns
                            + " wol="
                            + (wol ? 1 : 0));
            if (!"wlan0".equals(iface) || mtu != 1500 || dns != 0 || links != 0 || routes != 0
                    || proxy || nat64 || !"null".equals(domains) || !"null".equals(pdns) || wol) {
                throw new IllegalStateException(
                        "unexpected link properties iface="
                                + iface
                                + " mtu="
                                + mtu
                                + " dns="
                                + dns
                                + " links="
                                + links
                                + " routes="
                                + routes
                                + " proxy="
                                + proxy
                                + " nat64="
                                + nat64
                                + " domains="
                                + domains
                                + " pdns="
                                + pdns
                                + " wol="
                                + wol);
            }
            net.setText(
                    "net wifi="
                            + (wifi ? 1 : 0)
                            + " validated="
                            + (validated ? 1 : 0)
                            + " iface="
                            + iface
                            + " mtu="
                            + mtu
                            + " dns="
                            + dns
                            + " routes="
                            + routes
                            + " proxy="
                            + (proxy ? "set" : "null")
                            + " nat64="
                            + (nat64 ? "set" : "null")
                            + " stacked="
                            + links);
        } catch (Throwable t) {
            Log.e(TAG, "net FAIL " + t, t);
            net.setText("net FAIL " + t);
        }
    }
}
