import android.app.Notification;
import android.app.PendingIntent;
import android.content.IIntentReceiver;
import android.content.IIntentSender;
import android.content.Intent;
import android.bluetooth.BluetoothDevice;
import android.content.pm.ActivityInfo;
import android.content.pm.ApplicationInfo;
import android.content.pm.InstrumentationInfo;
import android.content.pm.PackageInfo;
import android.content.pm.PermissionGroupInfo;
import android.content.pm.PermissionInfo;
import android.content.pm.ResolveInfo;
import android.content.pm.ServiceInfo;
import android.content.res.Configuration;
import android.net.LinkProperties;
import android.net.NetworkCapabilities;
import android.graphics.drawable.Icon;
import android.os.Bundle;
import android.os.IBinder;
import android.os.LocaleList;
import android.os.Parcel;
import android.os.Parcelable;
import android.os.PersistableBundle;

import java.lang.reflect.Field;
import java.util.ArrayList;
import java.util.List;
import java.util.Locale;

/**
 * W112-A real-Java parcel comparison harness.
 *
 * <p>Runs on the real Android 14 framework through the ACOS VM. For each
 * logical value it constructs the real framework object, calls the real
 * {@code writeToParcel} into a {@code Parcel} (backed by the ACOS parcel
 * natives), and prints the marshalled bytes as hex:
 *
 * <pre>write.&lt;name&gt;=&lt;hex&gt;</pre>
 *
 * <p>When invoked with {@code &lt;name&gt;=&lt;hex&gt;} arguments, it unmarshalls each
 * Rust-encoded payload, calls the real {@code CREATOR.createFromParcel}, and
 * checks the reconstructed fields against the freshly-built object. Those
 * checks print:
 *
 * <pre>read.&lt;name&gt;=ok</pre>
 *
 * <p>This is the differential oracle for {@code acos-services::parcelable}: the
 * Rust test compares the two byte streams and, in the reverse direction, feeds
 * its own bytes to the real reader.
 */
public final class ParcelableCompareMain {
    static void check(boolean cond, String msg) {
        if (!cond) {
            throw new AssertionError(msg);
        }
    }

    static Field field(Class<?> c, String name) throws Exception {
        for (Field f : c.getDeclaredFields()) {
            if (f.getName().equals(name)) {
                f.setAccessible(true);
                return f;
            }
        }
        throw new NoSuchFieldException(c.getName() + "." + name);
    }

    static void set(Object o, String name, Object v) throws Exception {
        field(o.getClass(), name).set(o, v);
    }

    static Object get(Object o, String name) throws Exception {
        return field(o.getClass(), name).get(o);
    }

    static String hex(byte[] bytes) {
        StringBuilder sb = new StringBuilder(bytes.length * 2);
        for (byte b : bytes) {
            sb.append(Character.forDigit((b >> 4) & 0xf, 16));
            sb.append(Character.forDigit(b & 0xf, 16));
        }
        return sb.toString();
    }

    static byte[] unhex(String s) {
        byte[] out = new byte[s.length() / 2];
        for (int i = 0; i < out.length; i++) {
            out[i] = (byte) Integer.parseInt(s.substring(i * 2, i * 2 + 2), 16);
        }
        return out;
    }

    // ------------------------------------------------------------------
    // cases
    // ------------------------------------------------------------------

    static Configuration configuration() throws Exception {
        Configuration c = new Configuration();
        c.fontScale = 1.25f;
        c.mcc = 310;
        c.mnc = 260;
        c.setLocales(new LocaleList(new Locale("en", "US")));
        set(c, "userSetLocale", Boolean.TRUE);
        c.touchscreen = 3;
        c.keyboard = 2;
        c.keyboardHidden = 1;
        c.hardKeyboardHidden = 1;
        c.navigation = 1;
        c.navigationHidden = 2;
        c.orientation = 2;
        c.screenLayout = 0x20;
        c.colorMode = 0;
        c.uiMode = 0x11;
        c.screenWidthDp = 411;
        c.screenHeightDp = 914;
        c.smallestScreenWidthDp = 411;
        c.densityDpi = 420;
        set(c, "compatScreenWidthDp", 411);
        set(c, "compatScreenHeightDp", 914);
        set(c, "compatSmallestScreenWidthDp", 411);
        Object wc = get(c, "windowConfiguration");
        check(wc != null, "Configuration.windowConfiguration is non-null");
        set(wc, "mBounds", new android.graphics.Rect(0, 0, 1080, 2400));
        set(wc, "mAppBounds", new android.graphics.Rect(0, 0, 1080, 2337));
        set(wc, "mMaxBounds", new android.graphics.Rect(0, 0, 1080, 2400));
        set(wc, "mWindowingMode", 1);
        set(wc, "mActivityType", 1);
        set(wc, "mAlwaysOnTop", 0);
        set(wc, "mRotation", 0);
        set(wc, "mDisplayWindowingMode", 1);
        set(wc, "mDisplayRotation", 0);
        set(c, "assetsSeq", 7);
        set(c, "seq", 3);
        set(c, "fontWeightAdjustment", 100);
        set(c, "mGrammaticalGender", 3);
        return c;
    }

    static PersistableBundle persistableBundle() {
        PersistableBundle b = new PersistableBundle();
        b.putString("name", "helloviews");
        return b;
    }


    /**
     * REL3P: a `PackageInfo` carrying the arrays `PackageInfo.writeToParcel`
     * writes with {@code writeString8Array} ({@code splitNames},
     * {@code requestedPermissions}) plus a {@code Signature[]}. This is the
     * shape a `getPackageInfo(pkg, 0x100024c0)` reply has, and the case that
     * desynchronised the reader (7 209 071 signatures, 7 676 737 936-byte
     * allocation) when ACOS encoded those arrays as String16.
     *
     * <p>{@code applicationInfo} stays null so the value is a pure function of
     * these fields and the byte comparison against the Rust encoder is exact.
     */
    static PackageInfo packageInfoArrays() throws Exception {
        PackageInfo pi = new PackageInfo();
        // Public fields directly, hidden ones (@hide on the SDK stubs, present
        // on the real framework) through `set`.
        pi.packageName = "dev.acos.fixtures.pmsprobe";
        pi.splitNames = new String[] {"config.xxhdpi"};
        pi.versionCode = 7;
        pi.versionName = "1.0";
        pi.applicationInfo = null;
        pi.firstInstallTime = 1L;
        pi.lastUpdateTime = 2L;
        pi.activities = null;
        pi.receivers = null;
        pi.services = null;
        pi.providers = null;
        pi.instrumentation = null;
        pi.permissions = null;
        pi.requestedPermissions =
                new String[] {"android.permission.INTERNET", "android.permission.VIBRATE"};
        pi.requestedPermissionsFlags = new int[] {1, 1};
        pi.signatures = new android.content.pm.Signature[] {
                new android.content.pm.Signature(
                        new byte[] {0x30, (byte) 0x82, 0x01, 0x0a, 0x0d, 0x0e})
        };
        pi.installLocation = 0;
        set(pi, "versionCodeMajor", 0);
        set(pi, "baseRevisionCode", 0);
        set(pi, "splitRevisionCodes", null);
        set(pi, "sharedUserId", null);
        set(pi, "sharedUserLabel", 0);
        set(pi, "gids", null);
        set(pi, "configPreferences", null);
        set(pi, "reqFeatures", null);
        set(pi, "featureGroups", null);
        set(pi, "attributions", null);
        set(pi, "isStub", Boolean.FALSE);
        set(pi, "coreApp", Boolean.FALSE);
        set(pi, "requiredForAllUsers", Boolean.FALSE);
        set(pi, "restrictedAccountType", null);
        set(pi, "requiredAccountType", null);
        set(pi, "overlayTarget", null);
        set(pi, "overlayCategory", null);
        set(pi, "overlayPriority", 0);
        set(pi, "mOverlayIsStatic", Boolean.FALSE);
        set(pi, "compileSdkVersion", 0);
        set(pi, "compileSdkVersionCodename", null);
        set(pi, "signingInfo", null);
        set(pi, "isApex", Boolean.FALSE);
        set(pi, "isActiveApex", Boolean.FALSE);
        return pi;
    }

    /**
     * REL3P2: the capabilities of ACOS's emulated default network, built from
     * the framework's own constants so the test also checks the cap bit
     * positions. The uids (ArraySet of UidRange), the SSID, the underlying
     * networks, the specifier and the transport info are null, the allowed /
     * administrator uids and the sub ids are the constructor's empty sets: the
     * offline values ACOS serves today.
     */
    static NetworkCapabilities networkCapabilities() throws Exception {
        NetworkCapabilities nc = new NetworkCapabilities();
        long caps = 0;
        for (String name : new String[] {
                "NET_CAPABILITY_NOT_METERED", "NET_CAPABILITY_INTERNET",
                "NET_CAPABILITY_NOT_RESTRICTED", "NET_CAPABILITY_TRUSTED",
                "NET_CAPABILITY_NOT_VPN", "NET_CAPABILITY_NOT_ROAMING",
                "NET_CAPABILITY_FOREGROUND", "NET_CAPABILITY_NOT_CONGESTED",
                "NET_CAPABILITY_NOT_SUSPENDED", "NET_CAPABILITY_NOT_VCN_MANAGED"}) {
            caps |= 1L << ((Integer) field(NetworkCapabilities.class, name).get(null)).intValue();
        }
        set(nc, "mNetworkCapabilities", caps);
        set(nc, "mForbiddenNetworkCapabilities", 0L);
        set(nc, "mTransportTypes",
                1L << ((Integer) field(NetworkCapabilities.class, "TRANSPORT_WIFI").get(null)).intValue());
        set(nc, "mLinkUpBandwidthKbps", 0);
        set(nc, "mLinkDownBandwidthKbps", 0);
        set(nc, "mNetworkSpecifier", null);
        set(nc, "mTransportInfo", null);
        set(nc, "mSignalStrength", Integer.MIN_VALUE);
        set(nc, "mUids", null);
        set(nc, "mSSID", null);
        set(nc, "mPrivateDnsBroken", Boolean.FALSE);
        set(nc, "mOwnerUid", 1000);
        set(nc, "mRequestorUid", 1000);
        set(nc, "mRequestorPackageName", "android");
        set(nc, "mUnderlyingNetworks", null);
        set(nc, "mEnterpriseId", 0);
        return nc;
    }

    /**
     * REL3P2: ACOS's emulated `LinkProperties` -- the wifi interface name and
     * the AOSP emulator's MTU, with every address, route and proxy list empty.
     */
    static LinkProperties linkProperties() {
        LinkProperties lp = new LinkProperties();
        lp.setInterfaceName("wlan0");
        lp.setMtu(1500);
        return lp;
    }

    /**
     * A `Bundle` carrying the `BluetoothDevice.EXTRA_RSSI` short extra. The
     * framework writes `putShort` as `VAL_SHORT` (tag 5) with a 4-byte int
     * payload, and `Intent.getShortExtra` casts the value to `java.lang.Short`.
     */
    static Bundle rssiBundle() {
        Bundle b = new Bundle();
        b.putShort("android.bluetooth.device.extra.RSSI", (short) -40);
        return b;
    }

    /**
     * A `Notification` in the MODNOTIF3 codec's shape: a resource small icon,
     * title/text in the extras bundle, a group key and the channel id, with an
     * optional content `PendingIntent`.
     */
    static Notification notification(PendingIntent contentIntent) throws Exception {
        Notification n = new Notification();
        n.when = 1700000000000L;
        set(n, "creationTime", 1700000000000L);
        set(n, "mSmallIcon",
                Icon.createWithResource("dev.acos.fixtures.notif", 0x7f010001));
        // The image's `Notification()` seeds a default `audioAttributes` and
        // `STREAM_DEFAULT`; the codec models the null/0 pair, so the case pins
        // them explicitly (every other optional field is null too).
        set(n, "audioStreamType", 0);
        set(n, "audioAttributes", null);
        n.number = 1;
        n.contentIntent = contentIntent;
        n.flags = 0x10; // FLAG_ONGOING_EVENT
        set(n, "mGroupKey", "fixture-group");
        Bundle extras = new Bundle();
        extras.putString("android.title", "hello");
        extras.putString("android.text", "world");
        n.extras = extras;
        n.visibility = 1;
        set(n, "mChannelId", "fixture-channel");
        set(n, "mTimeout", 0L);
        return n;
    }

    /**
     * The `IIntentSender` a `PendingIntent` wraps. A local `Binder` is enough:
     * `PendingIntent.writeToParcel` writes it with `writeStrongBinder` and the
     * system side only ever calls `send` on it.
     */
    static final class Sender extends IIntentSender.Stub {
        @Override
        public void send(int code, Intent intent, String resolvedType,
                IBinder whitelistToken, IIntentReceiver finishedReceiver,
                String requiredPermission, Bundle options) {
        }
    }

    interface Writer {
        void write(Parcel p) throws Exception;
    }

    /**
     * The `List<BluetoothDevice>` `IBluetooth.getBondedDevices` answers inside a
     * `SynchronousResultReceiver.Result`. `BluetoothDevice(String)` is the
     * `@hide` constructor the framework itself uses (`mAddressType` 0), so the
     * case builds it reflectively.
     */
    static List<BluetoothDevice> bondedDevices() throws Exception {
        java.lang.reflect.Constructor<BluetoothDevice> ctor =
                BluetoothDevice.class.getDeclaredConstructor(String.class);
        ctor.setAccessible(true);
        List<BluetoothDevice> devices = new ArrayList<>();
        devices.add(ctor.newInstance("11:22:33:44:55:66"));
        devices.add(ctor.newInstance("AA:BB:CC:DD:EE:FF"));
        return devices;
    }

    // ------------------------------------------------------------------
    // PIFILLIN: `Intent.fillIn` differential cases
    // ------------------------------------------------------------------
    //
    // Each case builds the *stored* intent (`a`) and the caller's fill-in
    // (`b`), calls the real `Intent.fillIn(b, flags)` and writes `a` with the
    // real `writeToParcel`. The Rust side builds the same two intents, runs
    // `acos_services::fill_in::fill_in_value` and writes the merged value with
    // `parcelable::write_intent`: the two byte streams must be identical.
    //
    // A non-null `Uri` is deliberately absent: this image's `Uri.writeToParcel`
    // body is not pinned by the dex, so `parcelable` refuses it instead of
    // guessing (`type`-only reaches the same data/type rule with String8).

    static Intent filledIn(Intent a, Intent b, int flags) {
        a.fillIn(b, flags);
        return a;
    }

    static Intent emptyFilledIn() {
        Intent a = new Intent();
        Intent b = new Intent("android.intent.action.VIEW");
        b.setType("text/plain");
        b.setIdentifier("id-1");
        b.setPackage("dev.acos.fixtures.pifillin");
        b.addCategory("android.intent.category.DEFAULT");
        b.addCategory("android.intent.category.BROWSABLE");
        b.putExtra("action", "from-fill-in");
        b.putExtra("count", 3);
        return filledIn(a, b, 0);
    }

    static Intent componentFilledIn(boolean allow) {
        Intent a = new Intent();
        a.setComponent(new android.content.ComponentName("dev.acos.fixtures", "Stored"));
        a.setAction("stored.action");
        Intent b = new Intent();
        b.setComponent(new android.content.ComponentName("dev.acos.fixtures", "Fill"));
        b.setAction("fill.action");
        return filledIn(a, b, allow ? Intent.FILL_IN_COMPONENT | Intent.FILL_IN_ACTION : 0);
    }

    static Intent overwrittenFilledIn() {
        Intent a = new Intent();
        a.setAction("stored.action");
        a.addCategory("stored.category");
        a.setPackage("stored.pkg");
        a.setComponent(new android.content.ComponentName("stored.pkg", "Stored"));
        Intent b = new Intent();
        b.setAction("fill.action");
        b.addCategory("fill.category");
        b.setPackage("fill.pkg");
        b.setComponent(new android.content.ComponentName("fill.pkg", "Fill"));
        return filledIn(a, b, Intent.FILL_IN_ACTION | Intent.FILL_IN_CATEGORIES
                | Intent.FILL_IN_PACKAGE | Intent.FILL_IN_COMPONENT);
    }

    static Intent extrasFilledIn() {
        Intent a = new Intent();
        a.putExtra("stored", "mine");
        a.putExtra("shared", 1);
        Intent b = new Intent();
        b.putExtra("fresh", true);
        b.putExtra("shared", 2);
        return filledIn(a, b, 0);
    }

    static Intent dataTypeFilledIn() {
        Intent a = new Intent();
        a.setType("text/plain");
        Intent b = new Intent();
        b.setType("image/png");
        return filledIn(a, b, Intent.FILL_IN_DATA);
    }

    static Intent selectorFilledIn() {
        Intent a = new Intent();
        a.setAction("outer.action");
        Intent selector = new Intent("inner.action");
        selector.addCategory("inner.category");
        Intent b = new Intent();
        b.setSelector(selector);
        return filledIn(a, b, Intent.FILL_IN_SELECTOR);
    }

    static Intent flagsAndBoundsFilledIn() {
        Intent a = new Intent();
        a.setAction("stored.action");
        a.setFlags(0x10000000);
        a.setSourceBounds(new android.graphics.Rect(1, 2, 3, 4));
        Intent b = new Intent();
        b.setFlags(0x00000001);
        b.setSourceBounds(new android.graphics.Rect(10, 20, 30, 40));
        return filledIn(a, b, Intent.FILL_IN_SOURCE_BOUNDS);
    }

    static void emit(String name, Writer writer) throws Exception {
        Parcel p = Parcel.obtain();
        try {
            writer.write(p);
            System.out.println("write." + name + "=" + hex(p.marshall()));
        } finally {
            p.recycle();
        }
    }

    public static void main(String[] args) throws Exception {
        emit("configuration", p -> configuration().writeToParcel(p, 0));
        emit("persistable_bundle", p -> persistableBundle().writeToParcel(p, 0));
        emit("package_info_arrays", p -> packageInfoArrays().writeToParcel(p, 0));
        emit("network_capabilities", p -> networkCapabilities().writeToParcel(p, 0));
        emit("link_properties", p -> linkProperties().writeToParcel(p, 0));
        emit("bundle_short", p -> rssiBundle().writeToParcel(p, 0));
        // `SynchronousResultReceiver$Result.writeToParcel` (image dex):
        // `writeValue(mObject)` then `writeValue(mException)`. The Bluetooth
        // module answers `getBondedDevices` with a `List<BluetoothDevice>` and
        // `getState` with an `Integer`; both cases are emitted here so the Rust
        // `Result` codec can be byte-compared against the real writer.
        emit("bluetooth_result", p -> {
            p.writeValue(bondedDevices());
            p.writeValue(null);
        });
        emit("bluetooth_state", p -> {
            p.writeValue(Integer.valueOf(12));
            p.writeValue(null);
        });
        emit("notification", p -> notification(null).writeToParcel(p, 0));
        // The guest allocates the binder handle, so the harness reports the one
        // the framework itself wrote; the Rust side builds the same
        // `PendingIntent` with it and byte-compares the whole notification.
        final Sender sender = new Sender();
        emit("intent_sender", p -> p.writeStrongBinder(sender));
        // `PendingIntent(IIntentSender)` is `@hide`, so the public SDK stub does
        // not declare it; the runtime class does, and that is the constructor
        // the framework itself uses to rebuild a PendingIntent from a binder.
        final Class<?> iis = Class.forName("android.content.IIntentSender");
        final java.lang.reflect.Constructor<?> ctor =
                PendingIntent.class.getDeclaredConstructor(iis);
        ctor.setAccessible(true);
        final PendingIntent pending = (PendingIntent) ctor.newInstance(sender);
        emit("notification_intent", p -> notification(pending).writeToParcel(p, 0));
        // PIFILLIN: `a.fillIn(b, flags)` then `a.writeToParcel`.
        emit("fillin_empty", p -> emptyFilledIn().writeToParcel(p, 0));
        emit("fillin_component_kept", p -> componentFilledIn(false).writeToParcel(p, 0));
        emit("fillin_component_allowed", p -> componentFilledIn(true).writeToParcel(p, 0));
        emit("fillin_overwritten", p -> overwrittenFilledIn().writeToParcel(p, 0));
        emit("fillin_extras", p -> extrasFilledIn().writeToParcel(p, 0));
        emit("fillin_data_type", p -> dataTypeFilledIn().writeToParcel(p, 0));
        emit("fillin_selector", p -> selectorFilledIn().writeToParcel(p, 0));
        emit("fillin_flags_bounds", p -> flagsAndBoundsFilledIn().writeToParcel(p, 0));

        for (String arg : args) {
            int eq = arg.indexOf('=');
            if (eq < 0) {
                continue;
            }
            String name = arg.substring(0, eq);
            byte[] bytes = unhex(arg.substring(eq + 1));
            Parcel p = Parcel.obtain();
            try {
                p.unmarshall(bytes, 0, bytes.length);
                p.setDataPosition(0);
                Parcelable got = null;
                Object dynamicObject = null;
                Object dynamicException = null;
                if (name.startsWith("fillin_")) {
                    Intent intent = Intent.CREATOR.createFromParcel(p);
                    p.enforceNoDataAvail();
                    System.out.println("read." + name + "=ok");
                    System.out.println("read." + name + ".action="
                            + intent.getAction() + " type=" + intent.getType()
                            + " flags=" + intent.getFlags()
                            + " categories=" + intent.getCategories()
                            + " package=" + intent.getPackage()
                            + " extras=" + intent.getExtras());
                    Parcel rewritten = Parcel.obtain();
                    try {
                        intent.writeToParcel(rewritten, 0);
                        System.out.println("roundtrip." + name + "="
                                + hex(rewritten.marshall()));
                    } finally {
                        rewritten.recycle();
                    }
                    continue;
                }
                switch (name) {
                    case "bluetooth_result": {
                        dynamicObject = p.readValue(null);
                        dynamicException = p.readValue(null);
                        check(dynamicException == null, "bluetooth_result exception is null");
                        check(dynamicObject instanceof List,
                                "bluetooth_result is a List: " + dynamicObject);
                        List<?> devices = (List<?>) dynamicObject;
                        check(devices.size() == 2, "bluetooth_result size " + devices.size());
                        BluetoothDevice first = (BluetoothDevice) devices.get(0);
                        check("11:22:33:44:55:66".equals(first.getAddress()),
                                "bluetooth_result address " + first.getAddress());
                        BluetoothDevice second = (BluetoothDevice) devices.get(1);
                        check("AA:BB:CC:DD:EE:FF".equals(second.getAddress()),
                                "bluetooth_result second address " + second.getAddress());
                        break;
                    }
                    case "bluetooth_state": {
                        dynamicObject = p.readValue(null);
                        dynamicException = p.readValue(null);
                        check(dynamicException == null, "bluetooth_state exception is null");
                        check(Integer.valueOf(12).equals(dynamicObject),
                                "bluetooth_state object " + dynamicObject);
                        break;
                    }
                    case "configuration":
                        got = Configuration.CREATOR.createFromParcel(p);
                        Configuration c = (Configuration) got;
                        check(c.fontScale == 1.25f, "config fontScale");
                        check(c.mcc == 310, "config mcc");
                        check(c.mnc == 260, "config mnc");
                        check(c.getLocales().size() == 1
                                && c.getLocales().get(0).getLanguage().equals("en")
                                && c.getLocales().get(0).getCountry().equals("US"),
                                "config locale " + c.getLocales());
                        check(c.screenWidthDp == 411, "config width");
                        check(c.screenHeightDp == 914, "config height");
                        check(c.densityDpi == 420, "config density");
                        check(c.colorMode == 0, "config colorMode");
                        check(c.uiMode == 0x11, "config uiMode");
                        check(get(c, "assetsSeq").equals(7), "config assetsSeq");
                        check(get(c, "seq").equals(3), "config seq");
                        check(get(c, "fontWeightAdjustment").equals(100), "config fwa");
                        Object wc = get(c, "windowConfiguration");
                        check(((android.graphics.Rect) get(wc, "mBounds")).top == 0
                                && ((android.graphics.Rect) get(wc, "mBounds")).right == 1080,
                                "windowConfiguration mBounds");
                        check(get(wc, "mAppBounds") != null, "windowConfiguration mAppBounds");
                        check(((Integer) get(wc, "mWindowingMode")) == 1, "windowConfiguration mode");
                        break;
                    case "persistable_bundle":
                        got = PersistableBundle.CREATOR.createFromParcel(p);
                        check(((PersistableBundle) got).getString("name").equals("helloviews"),
                                "bundle name");
                        break;
                    case "bundle_short":
                        got = Bundle.CREATOR.createFromParcel(p);
                        check(((Bundle) got).getShort("android.bluetooth.device.extra.RSSI")
                                        == (short) -40,
                                "bundle_short rssi "
                                        + ((Bundle) got).getShort(
                                                "android.bluetooth.device.extra.RSSI"));
                        break;
                    case "application_info":
                        got = ApplicationInfo.CREATOR.createFromParcel(p);
                        break;
                    case "activity_info":
                        got = ActivityInfo.CREATOR.createFromParcel(p);
                        break;
                    case "package_info":
                        got = PackageInfo.CREATOR.createFromParcel(p);
                        break;
                    case "package_info_provider": {
                        // WV2: the `webviewupdate` reply's `PackageInfo` — the
                        // exact value `WebViewUpdateService` writes from the
                        // installed `com.android.webview` record. The nested
                        // `ApplicationInfo` is the part the Java materialiser
                        // writes for `PackageManager` but the Rust codec writes
                        // for this peer; a field-order difference shows up here
                        // as a desynchronised `requestedPermissions` (the image
                        // OOMs allocating the bogus `String[]`) or as
                        // `enforceNoDataAvail()` seeing leftover bytes.
                        got = PackageInfo.CREATOR.createFromParcel(p);
                        PackageInfo pi = (PackageInfo) got;
                        check("com.android.webview".equals(pi.packageName),
                                "package_info_provider packageName " + pi.packageName);
                        check(pi.versionCode == 1,
                                "package_info_provider versionCode " + pi.versionCode);
                        check(pi.applicationInfo != null,
                                "package_info_provider applicationInfo is null");
                        check("com.android.webview".equals(pi.applicationInfo.packageName),
                                "package_info_provider applicationInfo.packageName "
                                        + pi.applicationInfo.packageName);
                        check(pi.applicationInfo.sourceDir.endsWith(".apk"),
                                "package_info_provider sourceDir "
                                        + pi.applicationInfo.sourceDir);
                        check(pi.applicationInfo.uid > 0,
                                "package_info_provider uid " + pi.applicationInfo.uid);
                        check(pi.requestedPermissions != null
                                        && pi.requestedPermissions.length == 1
                                        && "android.permission.INTERNET".equals(
                                                pi.requestedPermissions[0]),
                                "package_info_provider requestedPermissions "
                                        + java.util.Arrays.toString(pi.requestedPermissions));
                        check(pi.splitNames == null,
                                "package_info_provider splitNames "
                                        + java.util.Arrays.toString(pi.splitNames));
                        break;
                    }
                    case "package_info_arrays":
                        got = PackageInfo.CREATOR.createFromParcel(p);
                        PackageInfo pi = (PackageInfo) got;
                        check(pi.packageName.equals("dev.acos.fixtures.pmsprobe"),
                                "package_info_arrays packageName " + pi.packageName);
                        check(pi.splitNames != null && pi.splitNames.length == 1
                                && pi.splitNames[0].equals("config.xxhdpi"),
                                "package_info_arrays splitNames " + java.util.Arrays.toString(pi.splitNames));
                        check(pi.requestedPermissions != null && pi.requestedPermissions.length == 2
                                && pi.requestedPermissions[0].equals("android.permission.INTERNET")
                                && pi.requestedPermissions[1].equals("android.permission.VIBRATE"),
                                "package_info_arrays requestedPermissions "
                                        + java.util.Arrays.toString(pi.requestedPermissions));
                        check(pi.requestedPermissionsFlags != null
                                && pi.requestedPermissionsFlags.length == 2
                                && pi.requestedPermissionsFlags[0] == 1
                                && pi.requestedPermissionsFlags[1] == 1,
                                "package_info_arrays requestedPermissionsFlags "
                                        + java.util.Arrays.toString(pi.requestedPermissionsFlags));
                        check(pi.signatures != null && pi.signatures.length == 1,
                                "package_info_arrays signatures "
                                        + (pi.signatures == null ? "null" : pi.signatures.length));
                        check(pi.versionCode == 7, "package_info_arrays versionCode");
                        break;
                    case "network_capabilities":
                        got = NetworkCapabilities.CREATOR.createFromParcel(p);
                        NetworkCapabilities nc = (NetworkCapabilities) got;
                        check(nc.hasCapability(NetworkCapabilities.NET_CAPABILITY_INTERNET),
                                "network_capabilities INTERNET");
                        check(nc.hasCapability((Integer) field(NetworkCapabilities.class,
                                        "NET_CAPABILITY_NOT_VCN_MANAGED").get(null)),
                                "network_capabilities NOT_VCN_MANAGED");
                        check(!nc.hasCapability(NetworkCapabilities.NET_CAPABILITY_VALIDATED),
                                "network_capabilities VALIDATED must be off");
                        check(nc.hasTransport(NetworkCapabilities.TRANSPORT_WIFI),
                                "network_capabilities WIFI transport");
                        check(nc.getLinkUpstreamBandwidthKbps() == 0
                                        && nc.getLinkDownstreamBandwidthKbps() == 0,
                                "network_capabilities bandwidth");
                        check(nc.getSignalStrength() == Integer.MIN_VALUE,
                                "network_capabilities signalStrength "
                                        + nc.getSignalStrength());
                        check(nc.getNetworkSpecifier() == null,
                                "network_capabilities specifier must be null");
                        check(nc.getTransportInfo() == null,
                                "network_capabilities transportInfo must be null");
                        check(nc.getEnterpriseIds().length == 0,
                                "network_capabilities enterprise ids");
                        check(nc.getOwnerUid() == 1000, "network_capabilities ownerUid");
                        break;
                    case "link_properties":
                        got = LinkProperties.CREATOR.createFromParcel(p);
                        LinkProperties lp = (LinkProperties) got;
                        check("wlan0".equals(lp.getInterfaceName()),
                                "link_properties interface " + lp.getInterfaceName());
                        check(lp.getMtu() == 1500, "link_properties mtu " + lp.getMtu());
                        check(lp.getLinkAddresses().isEmpty(), "link_properties link addresses");
                        check(lp.getDnsServers().isEmpty(), "link_properties dns servers");
                        check(lp.getDomains() == null, "link_properties domains");
                        check(lp.getRoutes().isEmpty(), "link_properties routes");
                        check(lp.getHttpProxy() == null, "link_properties http proxy");
                        check(lp.getNat64Prefix() == null, "link_properties nat64 prefix");
                        check(lp.isWakeOnLanSupported() == false, "link_properties wake on lan");
                        check(!lp.isWakeOnLanSupported(), "link_properties wake on lan");
                        break;
                    case "service_info":
                        got = ServiceInfo.CREATOR.createFromParcel(p);
                        check(((ServiceInfo) got).permission.equals("dev.acos.fixtures.ECHO"),
                                "service_info permission");
                        check(((ServiceInfo) got).flags == 0, "service_info flags");
                        check(((ServiceInfo) got).applicationInfo != null,
                                "service_info applicationInfo");
                        break;
                    case "instrumentation_info":
                        got = InstrumentationInfo.CREATOR.createFromParcel(p);
                        check(((InstrumentationInfo) got).targetPackage.equals("dev.acos.fixtures"),
                                "instrumentation_info targetPackage");
                        check(((InstrumentationInfo) got).functionalTest, "instrumentation_info functionalTest");
                        break;
                    case "permission_info":
                        got = PermissionInfo.CREATOR.createFromParcel(p);
                        check(((PermissionInfo) got).group.equals("android.permission-group.NETWORK"),
                                "permission_info group");
                        break;
                    case "permission_group_info":
                        got = PermissionGroupInfo.CREATOR.createFromParcel(p);
                        check(((PermissionGroupInfo) got).name.equals(
                                        "android.permission-group.CAMERA"),
                                "permission_group_info name");
                        check(((PermissionGroupInfo) got).loadLabel(null).toString().equals(
                                        "Camera"),
                                "permission_group_info nonLocalizedLabel");
                        check(((PermissionGroupInfo) got).descriptionRes == 0,
                                "permission_group_info descriptionRes");
                        check(((PermissionGroupInfo) got).priority == 700,
                                "permission_group_info priority");
                        break;
                    case "resolve_info":
                        got = ResolveInfo.CREATOR.createFromParcel(p);
                        check(((ResolveInfo) got).activityInfo != null, "resolve_info activityInfo");
                        check(((ResolveInfo) got).filter == null, "resolve_info filter is null");
                        break;
                    case "notification":
                        got = Notification.CREATOR.createFromParcel(p);
                        Notification n = (Notification) got;
                        check(n.extras != null, "notification extras");
                        check("hello".equals(n.extras.getString("android.title")),
                                "notification title");
                        check("world".equals(n.extras.getString("android.text")),
                                "notification text");
                        check("fixture-channel".equals(get(n, "mChannelId")),
                                "notification channel");
                        check(n.contentIntent == null, "notification contentIntent is null");
                        check(n.number == 1, "notification number");
                        check(n.flags == 0x10, "notification flags");
                        check(get(n, "mSmallIcon") != null, "notification small icon");
                        break;
                    default:
                        throw new AssertionError("unknown case " + name);
                }
                p.enforceNoDataAvail();
                System.out.println("read." + name + "=ok");
                // Differential: re-emit through the real writer. The bytes must
                // match the Rust encoder's byte-for-byte when the object is a
                // pure function of the parsed fields (no hidden defaults).
                Parcel rewritten = Parcel.obtain();
                try {
                    if (got != null) {
                        got.writeToParcel(rewritten, 0);
                    } else {
                        rewritten.writeValue(dynamicObject);
                        rewritten.writeValue(dynamicException);
                    }
                    System.out.println("roundtrip." + name + "=" + hex(rewritten.marshall()));
                } finally {
                    rewritten.recycle();
                }
            } finally {
                p.recycle();
            }
        }
        System.out.println("parcelcompare=ok");
    }
}
