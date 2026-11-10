import android.content.res.Configuration;
import android.os.LocaleList;
import android.os.Parcel;
import android.os.Parcelable;
import android.os.PersistableBundle;

import java.lang.reflect.Field;
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

    interface Writer {
        void write(Parcel p) throws Exception;
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
                Parcelable got;
                switch (name) {
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
                    default:
                        throw new AssertionError("unknown case " + name);
                }
                p.enforceNoDataAvail();
                System.out.println("read." + name + "=ok");
            } finally {
                p.recycle();
            }
        }
        System.out.println("parcelcompare=ok");
    }
}
