import android.content.Context;
import android.content.res.AssetManager;
import android.content.res.Configuration;
import android.content.res.Resources;
import android.content.res.TypedArray;
import android.util.AttributeSet;
import android.util.DisplayMetrics;
import android.view.LayoutInflater;
import android.view.View;
import android.view.ViewConfiguration;
import android.view.ViewGroup;
import android.widget.TextView;

import com.android.internal.policy.PhoneLayoutInflater;

/**
 * Java-level acceptance for layout inflation through the real
 * `android.view.LayoutInflater`.
 *
 * Runs on the real Android 14 core library + framework DEX. The APK under test
 * is passed as argv[0] (the fixture `hello-views.apk`). The case builds a real
 * `Resources`/`Theme`, a minimal `Context` backed by those resources, and
 * inflates the APK's compiled `res/layout/main.xml` through
 * `PhoneLayoutInflater`. It then walks the resulting view tree and prints every
 * node's class, resolved id, text and dimension attributes. Nothing is
 * synthesized on the Rust side; every value comes from the real framework
 * bytecode reading the compiled resource table and AXML.
 */
public class InflateMain {
    static Resources sRes;
    static Resources.Theme sTheme;

    /**
     * A `Context` whose resources and theme are the real ones built over the
     * fixture APK. `android.test.mock.MockContext` supplies the ~90 abstract
     * `Context` methods; the resource/theme/layout entry points are the ones
     * the inflation path actually uses.
     */
    static class TestContext extends android.test.mock.MockContext {
        LayoutInflater inflater;

        @Override
        public Resources getResources() {
            return sRes;
        }

        @Override
        public Resources.Theme getTheme() {
            if (sTheme == null) {
                sTheme = sRes.newTheme();
                sTheme.applyStyle(android.R.style.Theme_DeviceDefault, false);
            }
            return sTheme;
        }

        @Override
        public ClassLoader getClassLoader() {
            return InflateMain.class.getClassLoader();
        }

        @Override
        public android.os.Looper getMainLooper() {
            return android.os.Looper.myLooper();
        }

        @Override
        public Context getApplicationContext() {
            return this;
        }

        @Override
        public int checkCallingOrSelfPermission(String permission) {
            return android.content.pm.PackageManager.PERMISSION_DENIED;
        }

        @Override
        public int checkPermission(String permission, int pid, int uid) {
            return android.content.pm.PackageManager.PERMISSION_DENIED;
        }

        @Override
        public int checkCallingPermission(String permission) {
            return android.content.pm.PackageManager.PERMISSION_DENIED;
        }

        // `Context.getUserId()` is hidden in the public SDK but present on the
        // real framework class; the runtime virtual dispatch still reaches it.
        public int getUserId() {
            return 0;
        }

        android.content.pm.PackageManager mPm;

        @Override
        public android.content.pm.PackageManager getPackageManager() {
            if (mPm == null) {
                // `EditText`'s `Editor` requires a non-null `PackageManager` to
                // exist. `android.test.mock.MockPackageManager` is the real
                // framework test double (all methods throw); inflation never
                // queries it (only text-selection does).
                try {
                    mPm = (android.content.pm.PackageManager)
                            Class.forName("android.test.mock.MockPackageManager").newInstance();
                } catch (Throwable t) {
                    mPm = null;
                }
            }
            return mPm;
        }

        @Override
        public String getPackageName() {
            return "dev.acos.fixtures.helloviews";
        }

        @Override
        public android.content.pm.ApplicationInfo getApplicationInfo() {
            android.content.pm.ApplicationInfo ai = new android.content.pm.ApplicationInfo();
            ai.packageName = getPackageName();
            ai.targetSdkVersion = 34;
            ai.minSdkVersion = 26;
            return ai;
        }

        @Override
        public String getSystemServiceName(Class<?> serviceClass) {
            // `Context.getSystemService(Class)` returns null when the name is
            // null; `Editor.getInputMethodManager()` handles that.
            return null;
        }

        @Override
        public Object getSystemService(String name) {
            if (Context.LAYOUT_INFLATER_SERVICE.equals(name)) {
                if (inflater == null) {
                    inflater = new PhoneLayoutInflater(this);
                }
                return inflater;
            }
            return null;
        }

        // `Context.obtainStyledAttributes(...)` is `final` in API 34 and
        // delegates to `getTheme()`, which this context provides.
    }

    public static void main(String[] args) throws Exception {
        // `View` reads `AccessibilityManager.getInstance(context)` and that
        // builds a `Handler` on the context's main looper. Prepare the current
        // thread's looper (no messages are ever dispatched in this test).
        android.os.Looper.prepare();
        String apk = args[0];
        String pkg = "dev.acos.fixtures.canvasshapes";

        AssetManager am = new AssetManager();
        am.addAssetPath(apk);
        DisplayMetrics metrics = new DisplayMetrics();
        metrics.density = 2.625f;
        metrics.densityDpi = 420;
        Configuration config = new Configuration();
        config.densityDpi = 420;
        sRes = new Resources(am, metrics, config);
        seedViewConfiguration(metrics);
        sTheme = sRes.newTheme();
        sTheme.applyStyle(android.R.style.Theme_DeviceDefault, false);

        TestContext ctx = new TestContext();
        LayoutInflater inflater =
                (LayoutInflater) ctx.getSystemService(Context.LAYOUT_INFLATER_SERVICE);
        int layoutId = sRes.getIdentifier("main", "layout", pkg);
        View root;
        try {
            root = inflater.inflate(layoutId, null);
        } catch (Throwable t) {
            System.out.println("INFLATE-FAILED " + t);
            for (Throwable c = t.getCause(); c != null; c = c.getCause()) {
                System.out.println("CAUSE " + c);
                StackTraceElement[] st = c.getStackTrace();
                for (int i = 0; i < st.length && i < 12; i++) {
                    System.out.println("  at " + st[i]);
                }
                if (c.getMessage() != null) {
                    System.out.println("  msg " + c.getMessage());
                }
            }
            System.out.println("done");
            return;
        }
        dump(root, 0);

        System.out.println("done");
    }

    /**
     * `ViewConfiguration(Context)` reaches the `window` `IWindowManager`
     * service through `WindowManagerGlobal` exactly once, to read
     * `hasNavigationBar`, only when `sHasPermanentMenuKeySet` is false. A
     * standalone VM test has no service transport, so set that flag (the real
     * framework would set it on a device that does not probe the service). Every
     * other `ViewConfiguration` value is computed by the real constructor.
     */
    static void seedViewConfiguration(DisplayMetrics metrics) throws Exception {
        // Force `<clinit>` (a class literal does not initialize the class).
        ViewConfiguration.getAmbiguousGestureMultiplier();
        java.lang.reflect.Field f =
                ViewConfiguration.class.getDeclaredField("sConfigurations");
        f.setAccessible(true);
        @SuppressWarnings("unchecked")
        android.util.SparseArray<Object> map =
                (android.util.SparseArray<Object>) f.get(null);
        int key = (int) (metrics.density * 100);
        map.put(key, new ViewConfiguration());
    }

    static void dump(View v, int depth) {
        String indent = "";
        for (int i = 0; i < depth; i++) {
            indent += "  ";
        }
        String id = "0";
        if (v.getId() != View.NO_ID) {
            try {
                id = sRes.getResourceEntryName(v.getId());
            } catch (Resources.NotFoundException e) {
                id = "?" + v.getId();
            }
        }
        System.out.println(indent + "view=" + v.getClass().getName()
                + " id=" + id
                + " pad=" + v.getPaddingLeft()
                + " w=" + v.getWidth()
                + " h=" + v.getHeight());
        if (v instanceof TextView) {
            TextView tv = (TextView) v;
            System.out.println(indent + "  text=" + tv.getText());
            System.out.println(indent + "  hint=" + tv.getHint());
            System.out.println(indent + "  textSize=" + tv.getTextSize());
        }
        if (v instanceof ViewGroup) {
            ViewGroup g = (ViewGroup) v;
            for (int i = 0; i < g.getChildCount(); i++) {
                dump(g.getChildAt(i), depth + 1);
            }
        }
    }
}
