// W101-A pipeline-generated render harness.
//
// Runs the real Android 14 framework (`--corelib real`) with no `Activity`
// launch: it builds a real `Resources`/`Theme` over a fixture APK, inflates the
// APK's compiled layout XML through the real `PhoneLayoutInflater`, performs a
// real `View.measure`/`View.layout` at the ART oracle's 1080x2400 @ 420 dpi
// geometry, and then draws the resulting `View` hierarchy into a real
// `RecordingCanvas` (`View.draw`, not a hand-authored op list). The recorded
// `RenderNode` display list is rasterised by the real `HardwareRenderer` state
// machine, which in ACOS bottoms out in `acos-render`.
//
// The candidate frame therefore comes from the framework's own draw calls; the
// Rust test reads it from the FrameSink and diffs it against the ART
// screenshot. Nothing on the Rust side synthesises geometry or text.
//
// Output (stdout, machine-checkable):
//   sync=<code>
//   views=<count>
//   done
import android.content.Context;
import android.content.res.AssetManager;
import android.content.res.Configuration;
import android.content.res.Resources;
import android.graphics.HardwareRenderer;
import android.graphics.RecordingCanvas;
import android.graphics.RenderNode;
import android.util.DisplayMetrics;
import android.view.LayoutInflater;
import android.view.View;
import android.view.ViewConfiguration;
import android.view.ViewGroup;

import com.android.internal.policy.PhoneLayoutInflater;

public class RenderViewsMain {
    static Resources sRes;
    static Resources.Theme sTheme;
    static String sPkg;

    /**
     * Platform theme when the app manifest declares no {@code android:theme}.
     * The ART oracle was captured with {@code dark_light: no} (light mode), so
     * the effective device default is the light theme with a dark action bar.
     * (The framework's dark {@code Theme.DeviceDefault} is the night-mode
     * variant; a device with a light UI mode applies the light device
     * default.)
     */
    static int deviceDefaultTheme() {
        return android.R.style.Theme_DeviceDefault_Light_DarkActionBar;
    }

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
                sTheme.applyStyle(deviceDefaultTheme(), false);
            }
            return sTheme;
        }

        @Override
        public ClassLoader getClassLoader() {
            return RenderViewsMain.class.getClassLoader();
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

        public int getUserId() {
            return 0;
        }

        android.content.pm.PackageManager mPm;

        @Override
        public android.content.pm.PackageManager getPackageManager() {
            if (mPm == null) {
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
            return sPkg;
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
            return null;
        }

        // The real `android.test.mock.MockContext` throws here. A real Activity
        // context is not restricted, and `TextView.readTextAppearance` reads
        // this (API 34) while applying the resolved `textAppearance` bag.
        @Override
        public boolean isRestricted() {
            return false;
        }

        // `MockContext` also throws here; an app context returns false (only
        // system/unsafe resource loaders return true). The method is
        // `@hide`, so it is absent from `android.jar`: no `@Override`, but the
        // real `MockContext` is bound at runtime and dispatches here by
        // signature.
        public boolean canLoadUnsafeResources() {
            return false;
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
    }

    public static void main(String[] args) throws Exception {
        android.os.Looper.prepare();
        String apk = args[0];
        sPkg = args[1];
        String layout = args[2];
        int width = Integer.parseInt(args[3]);
        int height = Integer.parseInt(args[4]);
        String frameworkRes = args.length > 5 ? args[5] : null;

        AssetManager am = new AssetManager();
        // The app package only carries app resources; the platform drawables
        // (`android:drawable/btn_default_material`, …) live in framework-res.
        // A real device merges the framework package; add it explicitly here.
        if (frameworkRes != null) {
            am.addAssetPath(frameworkRes);
        }
        am.addAssetPath(apk);
        DisplayMetrics metrics = new DisplayMetrics();
        metrics.density = 2.625f;
        metrics.densityDpi = 420;
        metrics.scaledDensity = 2.625f;
        metrics.xdpi = 420f;
        metrics.ydpi = 420f;
        metrics.widthPixels = width;
        metrics.heightPixels = height;
        Configuration config = new Configuration();
        config.densityDpi = 420;
        sRes = new Resources(am, metrics, config);
        seedViewConfiguration(metrics);
        loadSystemFonts();
        sTheme = sRes.newTheme();
        sTheme.applyStyle(deviceDefaultTheme(), false);

        TestContext ctx = new TestContext();
        LayoutInflater inflater =
                (LayoutInflater) ctx.getSystemService(Context.LAYOUT_INFLATER_SERVICE);
        int layoutId = sRes.getIdentifier(layout, "layout", sPkg);
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

        // Real measure + layout at the device geometry (no Activity/ViewRootImpl).
        int wSpec = View.MeasureSpec.makeMeasureSpec(width, View.MeasureSpec.EXACTLY);
        int hSpec = View.MeasureSpec.makeMeasureSpec(height, View.MeasureSpec.EXACTLY);
        root.measure(wSpec, hSpec);
        root.layout(0, 0, width, height);
        if (root instanceof ViewGroup) {
            ViewGroup g = (ViewGroup) root;
            for (int i = 0; i < g.getChildCount(); i++) {
                View v = g.getChildAt(i);
                System.out.println("child[" + i + "]=" + v.getClass().getName()
                        + " " + v.getLeft() + "," + v.getTop() + ","
                        + v.getRight() + "," + v.getBottom());
            }
        }

        // Record the real `View.draw` calls into a RenderNode display list.
        RenderNode node = new RenderNode("acos-view-root");
        node.setPosition(0, 0, width, height);
        // The standalone harness has no display service, so a `RippleDrawable`
        // that starts its patterned background animation would die inside
        // `Choreographer.getRefreshRate`. The reference frame is static, so
        // stop those animations before recording (a real frame would run them).
        quiesceRippleAnimations(root);
        dumpViews(root, 0);
        RecordingCanvas canvas = node.beginRecording();
        // A device composites the activity window's `windowBackground` behind
        // the content view (`DecorView.draw`). This VM-level harness inflates
        // the layout alone, so the theme background would otherwise be missing
        // from every frame whose root has no background of its own. Draw it
        // through the real framework drawable, exactly as the DecorView does.
        drawWindowBackground(canvas, width, height);
        root.draw(canvas);
        node.endRecording();

        // Rasterise through the real HardwareRenderer state machine. ACOS's
        // implementation walks the RenderNode display list through acos-render
        // and submits the frame to the FrameSink.
        HardwareRenderer hr = new HardwareRenderer();
        hr.setContentRoot(node);
        hr.setSurface(null);
        int rc = hr.createRenderRequest().syncAndDraw();
        System.out.println("sync=" + rc);
        System.out.println("views=" + countViews(root));
        System.out.println("done");
    }

    static void dumpViews(View v, int depth) {
        StringBuilder sb = new StringBuilder();
        for (int i = 0; i < depth; i++) sb.append("  ");
        sb.append(v.getClass().getName())
          .append(' ').append(v.getLeft()).append(',').append(v.getTop())
          .append(',').append(v.getRight()).append(',').append(v.getBottom())
          .append(" alpha=").append(v.getAlpha())
          .append(" vis=").append(v.getVisibility());
        android.graphics.drawable.Drawable bg = v.getBackground();
        if (bg != null) {
            sb.append(" bg=").append(bg.getClass().getName())
              .append(bg.getBounds());
        }
        if (v instanceof android.widget.TextView) {
            android.widget.TextView tv = (android.widget.TextView) v;
            sb.append(" text=").append(tv.getText())
              .append(" color=").append(Integer.toHexString(tv.getCurrentTextColor()))
              .append(" textSize=").append(tv.getTextSize());
            android.text.Layout lay = tv.getLayout();
            if (lay == null) {
                sb.append(" layout=null");
            } else {
                sb.append(" layout=").append(lay.getClass().getSimpleName())
                  .append(" lines=").append(lay.getLineCount())
                  .append(" w=").append(lay.getWidth())
                  .append(" h=").append(lay.getHeight());
                if (lay.getLineCount() > 0) {
                    sb.append(" line0=[").append(lay.getLineStart(0)).append(',')
                      .append(lay.getLineEnd(0)).append(") top=").append(lay.getLineTop(0))
                      .append(" bot=").append(lay.getLineBottom(0));
                }
            }
        }
        System.out.println("VIEW " + sb);
        if (v instanceof ViewGroup) {
            ViewGroup g = (ViewGroup) v;
            for (int i = 0; i < g.getChildCount(); i++) {
                dumpViews(g.getChildAt(i), depth + 1);
            }
        }
    }

    /** Draw the theme's `windowBackground` behind the content, as `DecorView` does. */
    static void drawWindowBackground(RecordingCanvas canvas, int width, int height) {
        try {
            android.util.TypedValue tv = new android.util.TypedValue();
            if (!sTheme.resolveAttribute(android.R.attr.windowBackground, tv, true)
                    || tv.resourceId == 0) {
                return;
            }
            android.graphics.drawable.Drawable bg = sRes.getDrawable(tv.resourceId, sTheme);
            if (bg == null) {
                return;
            }
            bg.setBounds(0, 0, width, height);
            bg.draw(canvas);
        } catch (Throwable t) {
            System.out.println("window-bg-skip " + t);
        }
    }

    /**
     * Force any `RippleDrawable` background out of its pending patterned
     * animation. The static reference frame has no animation; without a display
     * service the animator's `Choreographer` construction NPEs.
     */
    static int quiesceRippleAnimations(View v) {
        int n = 0;
        android.graphics.drawable.Drawable bg = v.getBackground();
        if (bg != null
                && bg.getClass().getName().equals("android.graphics.drawable.RippleDrawable")) {
            try {
                java.lang.reflect.Field f =
                        bg.getClass().getDeclaredField("mRunBackgroundAnimation");
                f.setAccessible(true);
                f.setBoolean(bg, false);
                n++;
            } catch (Throwable t) {
                // No field on this framework build: nothing to quiesce.
            }
        }
        if (v instanceof ViewGroup) {
            ViewGroup g = (ViewGroup) v;
            for (int i = 0; i < g.getChildCount(); i++) {
                n += quiesceRippleAnimations(g.getChildAt(i));
            }
        }
        return n;
    }

    static int countViews(View v) {
        int n = 1;
        if (v instanceof ViewGroup) {
            ViewGroup g = (ViewGroup) v;
            for (int i = 0; i < g.getChildCount(); i++) {
                n += countViews(g.getChildAt(i));
            }
        }
        return n;
    }

    /**
     * `ViewConfiguration(Context)` reaches the `window` `IWindowManager`
     * service through `WindowManagerGlobal` exactly once, to read
     * `hasNavigationBar`, only when `sHasPermanentMenuKeySet` is false. A
     * standalone VM test has no service transport, so set that flag (the real
     * framework would set it on a device that does not probe the service).
     */
    /**
     * Populate `Typeface.sSystemFontMap` (and `Typeface.DEFAULT`). A real app
     * gets this from `ActivityThread.handleBindApplication`; the VM-level
     * harness has no ActivityThread, and API-34 lazy typeface initialization
     * leaves the map empty. `loadPreinstalledSystemFontMap` is `@hide`, so it
     * is reached reflectively. Without this, the now-resolved `textAppearance`
     * `fontFamily` makes `Typeface.create(String, int)` dereference a null
     * default.
     */
    static void loadSystemFonts() {
        try {
            java.lang.reflect.Method m = android.graphics.Typeface.class
                    .getMethod("loadPreinstalledSystemFontMap");
            m.invoke(null);
            System.out.println("fonts=loaded");
        } catch (Throwable t) {
            System.out.println("fonts-skip " + t);
        }
    }

    static void seedViewConfiguration(DisplayMetrics metrics) throws Exception {
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
}
