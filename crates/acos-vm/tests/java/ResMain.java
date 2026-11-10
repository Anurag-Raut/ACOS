import android.content.res.AssetManager;
import android.content.res.Configuration;
import android.content.res.Resources;
import android.content.res.TypedArray;
import android.content.res.XmlResourceParser;
import android.util.DisplayMetrics;
import android.util.TypedValue;
import java.io.InputStream;
import org.xmlpull.v1.XmlPullParser;

/**
 * Java-level acceptance for the `android.content.res` natives.
 *
 * Runs on the real Android 14 core library + framework DEX. The APK under test
 * is passed as argv[0] (the fixture `hello-views.apk`). Exercises:
 *
 *  - `new Resources(AssetManager)` over the APK;
 *  - `getIdentifier` + `getString(R.string.*)`;
 *  - `Theme.applyStyle` + `Theme.resolveAttribute(android.R.attr.textColor)`;
 *  - `getXml(R.layout.main)` and `obtainAttributes` on the real layout XML via
 *    an `XmlResourceParser` (no LayoutInflater).
 */
public class ResMain {
    public static void main(String[] args) throws Exception {
        String apk = args[0];
        String pkg = "dev.acos.fixtures.helloviews";

        AssetManager am = new AssetManager();
        am.addAssetPath(apk);

        DisplayMetrics metrics = new DisplayMetrics();
        metrics.density = 2.625f;
        metrics.densityDpi = 420;
        Configuration config = new Configuration();
        config.densityDpi = 420;

        Resources res = new Resources(am, metrics, config);

        int titleId = res.getIdentifier("title", "string", pkg);
        int appId = res.getIdentifier("app_name", "string", pkg);
        int missingId = res.getIdentifier("nope", "string", pkg);
        System.out.println("title=" + res.getString(titleId));
        System.out.println("app=" + res.getString(appId));
        System.out.println("missing=" + missingId);

        Resources.Theme theme = res.newTheme();
        theme.applyStyle(android.R.style.Theme_DeviceDefault, true);
        TypedValue tv = new TypedValue();
        // `Theme.DeviceDefault` does not define `android:attr/textColor` (only
        // the `textColorPrimary`/`textColorSecondary` family), so a faithful
        // `Theme.resolveAttribute` must report `false` and leave the
        // `TypedValue` untouched. Verified against real ART on the API-35
        // device and the API-34 `framework-res.apk` bag chain.
        boolean resolved = theme.resolveAttribute(android.R.attr.textColor, tv, true);
        System.out.println("resolve=" + resolved + " type=" + tv.type);
        // `colorBackground` *is* defined by the applied theme chain, as a
        // reference to a color resource, and must resolve with `resolve=true`.
        boolean bg = theme.resolveAttribute(android.R.attr.colorBackground, tv, true);
        System.out.println("bg=" + bg + " type=" + tv.type);
        // `textColorPrimary` is defined directly by `Theme.Material`.
        boolean primary = theme.resolveAttribute(android.R.attr.textColorPrimary, tv, true);
        System.out.println("primary=" + primary + " type=" + tv.type);

        int layoutId = res.getIdentifier("main", "layout", pkg);
        XmlResourceParser parser = res.getXml(layoutId);
        int ev;
        while ((ev = parser.next()) != XmlPullParser.START_TAG) {
            if (ev == XmlPullParser.END_DOCUMENT) {
                throw new IllegalStateException("no start tag");
            }
        }
        System.out.println("root=" + parser.getName());
        int[] attrs = {
            android.R.attr.orientation,
            android.R.attr.padding,
            android.R.attr.layout_width,
            android.R.attr.layout_height,
        };
        TypedArray ta = res.obtainAttributes(parser, attrs);
        System.out.println("orientation=" + ta.getInt(0, -1));
        System.out.println("padding=" + ta.getDimension(1, -1.0f));
        System.out.println("width=" + ta.getInt(2, -999));
        System.out.println("height=" + ta.getInt(3, -999));
        ta.recycle();
        parser.close();

        // Regression (W110-A): the static `nativeAsset*` methods receive only
        // the `AssetInputStream` pointer, but the engine's asset ids are
        // per-manager. A native that read a second `(manager, handle)` argument
        // crashed with an out-of-bounds register read as soon as a drawable
        // opened a real asset stream. Open the compiled layout and exercise
        // length / available / read / close end to end.
        InputStream asset = am.open("res/layout/main.xml");
        int assetLen = asset.available();
        byte[] head = new byte[4];
        int got = asset.read(head);
        asset.close();
        System.out.println("asset.ok=" + (assetLen > 0 && got == 4 && (head[0] & 0xff) == 0x03));

        System.out.println("done");
    }
}
