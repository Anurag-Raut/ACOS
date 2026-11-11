package dev.acos.e4h;

import android.content.Context;
import android.content.res.AssetManager;
import android.content.res.Configuration;
import android.content.res.Resources;
import android.content.res.XmlResourceParser;
import android.graphics.Bitmap;
import android.graphics.Canvas;
import android.graphics.RecordingCanvas;
import android.graphics.RenderNode;
import android.graphics.Color;
import android.graphics.drawable.ColorDrawable;
import android.graphics.Typeface;
import android.graphics.drawable.Drawable;
import android.text.TextPaint;
import android.util.DisplayMetrics;
import android.view.ContextThemeWrapper;
import android.view.LayoutInflater;
import android.view.View;
import android.view.ViewGroup;
import android.widget.FrameLayout;
import android.widget.TextView;

import java.io.File;
import java.io.FileOutputStream;
import java.lang.reflect.Constructor;
import java.lang.reflect.Field;
import java.lang.reflect.InvocationTargetException;
import java.lang.reflect.Method;
import java.util.ArrayList;
import java.util.List;

/**
 * E4H -- "M-B without a binder peer": real framework views inflated, measured,
 * laid out and drawn into a software Bitmap, in ONE dalvikvm64 process, using
 * only stock Google libraries plus the E2D/E2E shims.
 *
 * The rungs (each prints PASS/FAIL + the first blocker):
 *   gfx        register the graphics natives through the *exported* entry point
 *              (libhwui's register_android_graphics_classes) and prove it by
 *              calling HardwareRenderer.nSetIsSystemOrPersistent -- the exact
 *              native that stopped ActivityThread.systemMain() in E2D.
 *   runtime    register the libandroid_runtime natives the view path needs.
 *   activity   ActivityThread.systemMain() -> a real ContextImpl (SystemContext).
 *   resources  AssetManager over framework-res.apk + counter.apk, a Resources at
 *              1080x2400 / density 420, and a real Theme.
 *   inflate    LayoutInflater.inflate(res/layout/main.xml) under that theme.
 *   layout     measure/layout at 1080x2062 (the device's content area).
 *   draw       View.draw() into a software Bitmap via Canvas, PNG + tree dump.
 *
 *   dalvikvm -cp ... dev.acos.e4h.Main all
 */
public final class Main {

    static final String APK_PROP = "e4h.apk";
    static final String PKG_PROP = "e4h.pkg";
    static final String OUT_PROP = "e4h.out";
    static final int W = 1080;
    static final int H = 2400;
    static final int CONTENT_H = 2062;  // 2337-275: app content area on the oracle
    static final int DENSITY = 420;

    static int failures = 0;
    static final List<String> steps = new ArrayList<>();

    public static void main(String[] args) {
        String step = args.length > 0 ? args[0] : "all";
        System.out.println("== E4H step=" + step + " ==");
        long t0 = System.currentTimeMillis();
        try {
            // The ladder is monotone: each rung is cheap once the previous one
            // has run, and `draw` re-runs inflate+layout so the timings are real.
            stepGfx();
            stepRuntime();
            ensureActivity();
            stepResources();
            if (!step.equals("gfx") && !step.equals("runtime") && !step.equals("activity")
                    && !step.equals("resources")) {
                stepDraw();
            }
        } catch (Throwable t) {
            failures++;
            System.out.println("FATAL " + t);
            t.printStackTrace(System.out);
        }
        long ms = System.currentTimeMillis() - t0;
        System.out.println("TIME wall_ms=" + ms);
        System.out.println("RSS peak_kb=" + peakRssKb());
        System.out.println("== E4H done failures=" + failures + " wall_ms=" + ms + " ==");
        System.exit(failures == 0 ? 0 : 1);
    }

    static void ok(String s) { steps.add("PASS " + s); System.out.println("PASS " + s); }

    static void bad(String s, Throwable t) {
        failures++;
        steps.add("FAIL " + s + " : " + t);
        System.out.println("FAIL " + s + " : " + t);
        StackTraceElement[] st = t.getStackTrace();
        for (int i = 0; i < Math.min(6, st.length); i++) System.out.println("     at " + st[i]);
        Throwable c = t.getCause();
        while (c != null) { System.out.println("     cause: " + c); c = c.getCause(); }
    }

    static void note(String s) { System.out.println("NOTE " + s); }

    // ---------------------------------------------------------------- natives
    /**
     * E2D's shim (dev.acos.e2d.Nat, /data/dex/e2d/classes.dex) resolves and calls
     * the exported android::register_* entry points; the JNI symbol names are
     * bound to that class, so we call it rather than duplicating it.
     */
    static void loadNat() { dev.acos.e2d.Nat.load(); }

    static int natRegister(String plainName) { return dev.acos.e2d.Nat.register(plainName); }

    static final String[] GRAPHICS = {"register_android_graphics_classes"};

    static final String[] RUNTIME = {
            "register_android_util_Log",
            "register_android_os_SystemProperties",
            "register_android_os_SystemClock",
            "register_android_os_Trace",
            "register_android_os_Parcel",
            "register_android_os_Binder",
            "register_android_os_MessageQueue",
            "register_android_os_Process",
            "register_android_os_GraphicsEnvironment",
            "register_android_content_StringBlock",
            "register_android_content_XmlBlock",
            "register_android_content_AssetManager",
            "register_android_content_res_ApkAssets",
            "register_android_content_res_Configuration",
            // Choreographer (reached from AnimationHandler when a Drawable starts
            // an animation during draw) needs DisplayEventReceiver's natives; its
            // register function is a plain RegisterNatives with no class-init trap.
            "register_android_view_DisplayEventReceiver",
            "register_android_text_AndroidCharacter",
            "register_android_text_Hyphenator",
            "register_android_util_CharsetUtils",
            // NOTE: view_SurfaceControl/view_Surface/graphics_{GraphicBuffer,
            // BLASTBufferQueue,SurfaceTexture} are deliberately NOT registered:
            // their register function runs the class initialiser, whose <clinit>
            // calls a native of the *same* class that is not registered yet
            // (measured: SurfaceControl.nativeGetNativeSurfaceControlFinalizer
            // UnsatisfiedLinkError -> sticky NoClassDefFoundError). The software
            // draw path does not need a Surface, so we skip them.
    };

    static boolean gfxDone, runtimeDone;

    static boolean reg(String[] names, String tag) {
        int okc = 0, miss = 0;
        for (String n : names) {
            try {
                int rc = natRegister(n);
                if (rc == 1) okc++;
                else { miss++; note(tag + " register(" + n + ") -> " + rc); }
            } catch (Throwable t) {
                miss++;
                note(tag + " register(" + n + ") threw " + t);
            }
        }
        System.out.println("REG " + tag + " ok=" + okc + " missing=" + miss + " of " + names.length);
        return miss == 0;
    }

    /** Rung gfx: the E2D blocker -- HardwareRenderer.nSetIsSystemOrPersistent(Z). */
    static void stepGfx() {
        if (gfxDone) return;
        loadNat();
        try {
            boolean all = reg(GRAPHICS, "graphics");
            // Prove the natives are live by *calling* the one that blocked E2D.
            Class<?> hr = Class.forName("android.graphics.HardwareRenderer");
            Method m = null;
            for (Method cand : hr.getDeclaredMethods()) {
                if (cand.getName().equals("nSetIsSystemOrPersistent")) { m = cand; break; }
            }
            if (m == null) throw new NoSuchMethodException("HardwareRenderer.nSetIsSystemOrPersistent");
            m.setAccessible(true);
            m.invoke(null, Boolean.FALSE);
            ok("[gfx] libhwui register_android_graphics_classes -> HardwareRenderer.nSetIsSystemOrPersistent(false) OK"
               + " (register ok=" + all + ")");
            gfxDone = true;
        } catch (Throwable t) {
            bad("[gfx] register graphics natives", unwrap(t));
        }
    }

    static void stepRuntime() {
        if (runtimeDone) return;
        try {
            boolean all = reg(RUNTIME, "runtime");
            ok("[runtime] libandroid_runtime register_android_* (all=" + all + ")");
            runtimeDone = true;
        } catch (Throwable t) {
            bad("[runtime] register libandroid_runtime natives", unwrap(t));
        }
    }

    // ------------------------------------------------------------- ActivityThread
    static Object sActivityThread;
    static Context sSystemContext;

    static void ensureActivity() {
        if (sSystemContext != null) return;
        try {
            // systemMain() constructs ActivityThread$H, which needs a *main*
            // Looper on this thread. E2D proved the real Looper/MessageQueue
            // work over the memfd binder stand-in, so prepare it first.
            android.os.Looper.prepareMainLooper();
            Class<?> at = Class.forName("android.app.ActivityThread");
            Method systemMain = at.getDeclaredMethod("systemMain");
            systemMain.setAccessible(true);
            sActivityThread = systemMain.invoke(null);
            Method getSystemContext = at.getDeclaredMethod("getSystemContext");
            getSystemContext.setAccessible(true);
            sSystemContext = (Context) getSystemContext.invoke(sActivityThread);
            ok("[activity] ActivityThread.systemMain() -> " + sActivityThread
               + " getSystemContext() -> " + sSystemContext
               + " res=" + (sSystemContext == null ? null : sSystemContext.getResources()));
        } catch (Throwable t) {
            bad("[activity] ActivityThread.systemMain()", unwrap(t));
        }
    }

    // -------------------------------------------------------------- resources
    static Resources sRes;
    static Resources.Theme sTheme;
    static int sLayoutId;
    static String sPkg;

    static AssetManager newAssetManager() throws Exception {
        Constructor<AssetManager> c = AssetManager.class.getDeclaredConstructor();
        c.setAccessible(true);
        return c.newInstance();
    }

    static int addAssetPath(AssetManager am, String path) throws Exception {
        Method m = AssetManager.class.getDeclaredMethod("addAssetPath", String.class);
        m.setAccessible(true);
        return (Integer) m.invoke(am, path);
    }

    static void stepResources() {
        if (sRes != null) return;
        sPkg = System.getProperty(PKG_PROP, "dev.acos.fixtures.counter");
        final String apk = System.getProperty(APK_PROP, "/data/apk/counter.apk");
        try {
            AssetManager am = newAssetManager();
            int c1 = addAssetPath(am, "/system/framework/framework-res.apk");
            int c2 = addAssetPath(am, apk);
            DisplayMetrics dm = new DisplayMetrics();
            dm.widthPixels = W;
            dm.heightPixels = H;
            dm.densityDpi = DENSITY;
            dm.density = DENSITY / 160f;
            dm.scaledDensity = dm.density;
            dm.xdpi = 441.0f;
            dm.ydpi = 441.0f;
            Configuration cfg = new Configuration();
            cfg.densityDpi = DENSITY;
            cfg.screenWidthDp = 411;
            cfg.screenHeightDp = 842;
            cfg.smallestScreenWidthDp = 411;
            cfg.orientation = Configuration.ORIENTATION_PORTRAIT;
            cfg.screenLayout = Configuration.SCREENLAYOUT_LONG_NO
                    | Configuration.SCREENLAYOUT_SIZE_NORMAL;
            cfg.uiMode = Configuration.UI_MODE_TYPE_NORMAL | Configuration.UI_MODE_NIGHT_NO;
            Constructor<Resources> rc = Resources.class.getDeclaredConstructor(
                    AssetManager.class, DisplayMetrics.class, Configuration.class);
            rc.setAccessible(true);
            sRes = rc.newInstance(am, dm, cfg);
            // The 3-arg ctor recomputes DisplayMetrics from the Configuration; push
            // the real panel metrics back (this is what ResourcesManager does).
            try {
                Method uc = Resources.class.getDeclaredMethod(
                        "updateConfiguration", Configuration.class, DisplayMetrics.class);
                uc.setAccessible(true);
                uc.invoke(sRes, cfg, dm);
            } catch (Throwable t) {
                note("Resources.updateConfiguration -> " + t);
            }
            int appName = sRes.getIdentifier("app_name", "string", sPkg);
            int layout = sRes.getIdentifier("main", "layout", sPkg);
            sLayoutId = layout;
            ok("[resources] AssetManager cookies 0x" + Integer.toHexString(c1) + "/0x"
               + Integer.toHexString(c2) + " app_name=" + (appName == 0 ? "?" : sRes.getString(appName))
               + " layout/main=0x" + Integer.toHexString(layout)
               + " density=" + sRes.getDisplayMetrics().density
               + " " + sRes.getDisplayMetrics().widthPixels + "x" + sRes.getDisplayMetrics().heightPixels);
            if (layout == 0) throw new IllegalStateException("layout/main not found in " + apk);
            XmlResourceParser xp = sRes.getLayout(layout);
            String rootTag = null;
            for (int ev = xp.next(); ev != XmlResourceParser.END_DOCUMENT; ev = xp.next()) {
                if (ev == XmlResourceParser.START_TAG) { rootTag = xp.getName(); break; }
            }
            note("[resources] layout root tag <" + rootTag + ">");
        } catch (Throwable t) {
            bad("[resources] Resources over framework-res + apk", unwrap(t));
            return;
        }
        // Theme: the platform default (this is what Theme.DeviceDefault.Light is
        // built on and what the counter activity inherits: no android:theme in
        // its manifest).
        try {
            sTheme = sRes.newTheme();
            int themeId = android.R.style.Theme_DeviceDefault_Light;
            sTheme.applyStyle(themeId, true);
            ok("[resources] Theme = android:style/Theme.DeviceDefault.Light (0x"
               + Integer.toHexString(themeId) + ")");
        } catch (Throwable t) {
            bad("[resources] Theme.DeviceDefault.Light", unwrap(t));
        }
    }

    // ---------------------------------------------------------------- context
    /** A Context over our Resources/Theme, based on the SystemContext for the rest. */
    static final class MiniContext extends ContextThemeWrapper {
        private final Resources res;
        private final Resources.Theme th;
        private LayoutInflater inflater;

        MiniContext(Context base, Resources res, Resources.Theme th) {
            super(base, 0);
            this.res = res;
            this.th = th;
        }

        @Override public Resources getResources() { return res; }
        @Override public AssetManager getAssets() { return res.getAssets(); }
        @Override public Resources.Theme getTheme() { return th; }

        @Override public Object getSystemService(String name) {
            if (Context.LAYOUT_INFLATER_SERVICE.equals(name)) {
                if (inflater == null) {
                    // PhoneLayoutInflater is @hide; cloneInContext() gives us one
                    // bound to *this* context (that is exactly what ContextImpl does).
                    inflater = LayoutInflater.from(getBaseContext()).cloneInContext(this);
                }
                return inflater;
            }
            return super.getSystemService(name);
        }
    }

    /**
     * The one place the "no binder peer" cut hurts: View's constructor asks
     * ViewConfiguration, which asks IWindowManager.hasNavigationBar(displayId)
     * through WindowManagerGlobal -> ServiceManager -> binder. We have no
     * servicemanager, so install a reflective Proxy as the cached
     * WindowManagerGlobal.sWindowManagerService: default-answer every method
     * (false/0/null). This is the ACOS transport seam in miniature: in M-B a
     * real peer answers these, here only the *view* path needs 4 answers.
     */
    static void installStubWindowManager() {
        try {
            Class<?> iwm = Class.forName("android.view.IWindowManager");
            Object proxy = java.lang.reflect.Proxy.newProxyInstance(
                    iwm.getClassLoader(), new Class<?>[]{iwm},
                    (p, m, a) -> {
                        Class<?> r = m.getReturnType();
                        String n = m.getName();
                        System.out.println("STUB IWindowManager." + n
                                + " -> default(" + r.getSimpleName() + ")");
                        if (r == boolean.class) return Boolean.FALSE;
                        if (r == int.class) return 0;
                        if (r == long.class) return 0L;
                        if (r == float.class) return 0f;
                        if (r == double.class) return 0d;
                        if (r == short.class) return (short) 0;
                        if (r == byte.class) return (byte) 0;
                        if (r == char.class) return (char) 0;
                        return null;
                    });
            Class<?> wmg = Class.forName("android.view.WindowManagerGlobal");
            Field f = wmg.getDeclaredField("sWindowManagerService");
            f.setAccessible(true);
            f.set(null, proxy);
            ok("[services] WindowManagerGlobal.sWindowManagerService = reflective stub");
        } catch (Throwable t) {
            note("[services] stub window manager -> " + unwrap(t));
        }
    }

    static MiniContext newContext() {
        MiniContext ctx = new MiniContext(sSystemContext, sRes, sTheme);
        try {
            System.out.println("CTX application=" + ctx.getApplicationInfo().packageName
                               + " targetSdk=" + ctx.getApplicationInfo().targetSdkVersion);
        } catch (Throwable t) {
            note("CTX getApplicationInfo() -> " + t);
        }
        return ctx;
    }

    // ------------------------------------------------------------------- draw
    /**
     * Typeface's static initialiser builds sSystemFontMap from
     * /system/etc/fonts.xml (we provision it) but leaves sDefaults[0] null when
     * the "families with default" native returns 0 -- which is what a missing
     * font *config* looks like. TextView's constructor then dies on
     * Typeface.create((Typeface) null, NORMAL). Probe it, and if needed seed the
     * two static fields from a real font file.
     */
    static void stepText() {
        try {
            System.out.println("TEXT Typeface.DEFAULT=" + android.graphics.Typeface.DEFAULT
                    + " SANS=" + android.graphics.Typeface.SANS_SERIF
                    + " create(sans-serif)=" + android.graphics.Typeface.create("sans-serif", 0));
        } catch (Throwable t) {
            note("[text] Typeface statics -> " + unwrap(t));
        }
        try {
            Class<?> tf = Class.forName("android.graphics.Typeface");
            Field sdef = tf.getDeclaredField("sDefaultTypeface");
            sdef.setAccessible(true);
            Object cur = sdef.get(null);
            if (cur == null) {
                seedTypeface(tf.getDeclaredField("sDefaultTypeface"));
                android.graphics.Typeface roboto = android.graphics.Typeface.DEFAULT;
                android.graphics.Typeface bold = android.graphics.Typeface.DEFAULT_BOLD;
                Field sd = tf.getDeclaredField("sDefaults");
                sd.setAccessible(true);
                android.graphics.Typeface[] defaults = (android.graphics.Typeface[]) sd.get(null);
                if (defaults == null) defaults = new android.graphics.Typeface[4];
                for (int i = 0; i < defaults.length; i++) defaults[i] = roboto;
                defaults[1] = bold;
                sd.set(null, defaults);
                sdef.set(null, roboto);
                ok("[text] seeded Typeface.sDefaultTypeface/sDefaults from /system/fonts "
                   + "(createFromFile -> " + roboto + ", " + bold + ")");
            } else {
                ok("[text] Typeface.sDefaultTypeface already set: " + cur);
            }
            android.text.TextPaint tp = new android.text.TextPaint();
            tp.setTypeface(android.graphics.Typeface.DEFAULT);
            tp.setTextSize(28);
            ok("[text] measureText(\"Increment\") = " + tp.measureText("Increment")
               + " fontMetrics ascent=" + tp.getFontMetricsInt().ascent
               + " descent=" + tp.getFontMetricsInt().descent);
        } catch (Throwable t) {
            bad("[text] Typeface defaults", unwrap(t));
        }
    }

    /**
     * Typeface's own static initialiser leaves sSystemFontMap/sDefaults null in
     * this image: the map normally arrives through
     * Typeface.sSystemFontMapSharedMemory (a SharedMemory region that zygote /
     * system_server allocates), and without it the class falls back to nothing.
     * We rebuild exactly what zygote would have built: SystemFonts'
     * preinstalled FontConfig -> buildSystemTypefaces() -> the two static maps.
     */
    static void seedTypeface(Field sDefaultTypeface) throws Exception {
        Class<?> sf = Class.forName("android.graphics.fonts.SystemFonts");
        Method pre = sf.getDeclaredMethod("getSystemPreinstalledFontConfig");
        pre.setAccessible(true);
        Object fc = pre.invoke(null);
        System.out.println("SEED FontConfig=" + fc);

        Class<?> am = Class.forName("android.util.ArrayMap");
        Object map = am.getConstructor().newInstance();
        Method bst = sf.getDeclaredMethod("buildSystemTypefaces", Class.forName("android.text.FontConfig"),
                java.util.Map.class);
        bst.setAccessible(true);
        bst.invoke(null, fc, map);
        Object fallback = am.getConstructor().newInstance();
        try {
            Method bsf = sf.getDeclaredMethod("buildSystemFallback", Class.forName("android.text.FontConfig"),
                    am);
            bsf.setAccessible(true);
            bsf.invoke(null, fc, fallback);
        } catch (NoSuchMethodException e) {
            note("SEED buildSystemFallback(FontConfig,ArrayMap) missing");
        }

        Class<?> tf = Class.forName("android.graphics.Typeface");
        Field fMap = tf.getDeclaredField("sSystemFontMap");
        fMap.setAccessible(true);
        fMap.set(null, map);
        Field fFallback = tf.getDeclaredField("sSystemFallbackMap");
        fFallback.setAccessible(true);
        fFallback.set(null, fallback);
        Object sans = ((java.util.Map<?, ?>) map).get("sans-serif");
        System.out.println("SEED typefaces=" + ((java.util.Map<?, ?>) map).size()
                + " sans-serif=" + sans + " fallback=" + ((java.util.Map<?, ?>) fallback).size());
        if (sans == null) sans = makeDefaultTypeface();
        if (sans != null && ((java.util.Map<?, ?>) map).isEmpty()) {
            for (String n : new String[]{"sans-serif", "sans-serif-medium", "sans-serif-condensed",
                    "sans-serif-light", "sans-serif-thin", "roboto", "serif", "monospace"}) {
                ((java.util.Map) map).put(n, sans);
            }
        }
        if (sans == null) {
            // buildSystemTypefaces only emits *named* families and this image's
            // framework.jar build put none in the map; make the default from a
            // real FontFamily instead.
            sans = android.graphics.Typeface.createFromFile("/system/fonts/Roboto-Regular.ttf");
            System.out.println("SEED createFromFile -> " + sans);
            ((java.util.Map) map).put("sans-serif", sans);
            ((java.util.Map) map).put("sans-serif-medium", sans);
            ((java.util.Map) map).put("sans-serif-condensed", sans);
            ((java.util.Map) map).put("roboto", sans);
        }
        sDefaultTypeface.setAccessible(true);
        boolean done = false;
        // Typeface.setDefault(Typeface) is the @hide path zygote uses: it sets the
        // Java field *and* libhwui's native gDefaultTypeface, which is what the
        // native side asserts on (ASSERT src == nullptr && gDefaultTypeface == nullptr).
        for (Method cand : Typeface.class.getDeclaredMethods()) {
            if (cand.getName().equals("setDefault") && cand.getParameterCount() == 1) {
                try {
                    cand.setAccessible(true);
                    cand.invoke(null, sans);
                    System.out.println("SEED Typeface.setDefault(" + sans + ") (java+native)");
                    done = true;
                } catch (Throwable t) {
                    System.out.println("SEED Typeface.setDefault -> " + unwrap(t));
                }
            }
        }
        if (done) { /* default installed on both sides */ }
        else try {
            sDefaultTypeface.set(null, sans);
        } catch (IllegalAccessException e) {
            // ART refuses reflective *writes* to boot-classpath statics through
            // MethodHandles/Field unless the field is unfrozen; use Unsafe.
            Class<?> unsafe = Class.forName("sun.misc.Unsafe");
            Field uf = unsafe.getDeclaredField("theUnsafe");
            uf.setAccessible(true);
            Object u = uf.get(null);
            Field base = null;
            Method staticFieldBase = unsafe.getMethod("staticFieldBase", Field.class);
            Method staticFieldOffset = unsafe.getMethod("staticFieldOffset", Field.class);
            Method putReference = unsafe.getMethod("putReference", Object.class, long.class,
                    Object.class, Class.forName("java.lang.Object"));
            Object b = staticFieldBase.invoke(u, sDefaultTypeface);
            long off = (Long) staticFieldOffset.invoke(u, sDefaultTypeface);
            putReference.invoke(u, b, off, sans, Object.class);
            note("[text] sDefaultTypeface written through Unsafe");
        }
        Field fDefaults = tf.getDeclaredField("sDefaults");
        fDefaults.setAccessible(true);
        android.graphics.Typeface[] d = new android.graphics.Typeface[4];
        d[0] = (android.graphics.Typeface) sans;
        d[1] = d[0];
        d[2] = d[0];
        d[3] = d[0];
        try {
            fDefaults.set(null, d);
        } catch (IllegalAccessException e) {
            note("[text] sDefaults write refused: " + e);
        }
        ok("[text] seeded Typeface.sSystemFontMap (" + ((java.util.Map<?, ?>) map).size()
           + " families), sSystemFallbackMap (" + ((java.util.Map<?, ?>) fallback).size()
           + "), sDefaultTypeface=" + sans);
    }

    /**
     * Every high-level Typeface factory routes through CustomFallbackBuilder,
     * which throws on this image because sSystemFallbackMap/defaults are null.
     * The low-level ones (nativeCreateFromFamilies) do not, so try them in
     * order of preference and report which one worked.
     */
    static Typeface makeDefaultTypeface() {
        // The *modern* low-level factory: android.graphics.fonts.FontFamily (JNI
        // backed, proven to build) + Typeface.createFromFamilies(String,
        // FontFamily[]) -- this is the exact call SystemFonts.buildSystemTypefaces
        // makes, without going through CustomFallbackBuilder.
        String[] paths = {"/system/fonts/Roboto-Regular.ttf", "/system/fonts/RobotoStatic-Regular.ttf"};
        for (String path : paths) {
            try {
                Class<?> fb = Class.forName("android.graphics.fonts.Font$Builder");
                Object fontB = fb.getConstructor(java.io.File.class).newInstance(new java.io.File(path));
                Object font = fb.getMethod("build").invoke(fontB);
                Class<?> fontCls = Class.forName("android.graphics.fonts.Font");
                Class<?> ffamB = Class.forName("android.graphics.fonts.FontFamily$Builder");
                Object familyB = ffamB.getConstructor(fontCls).newInstance(font);
                Class<?> famCls = Class.forName("android.graphics.fonts.FontFamily");
                Object family = ffamB.getMethod("build").invoke(familyB);
                Object arr = java.lang.reflect.Array.newInstance(famCls, 1);
                java.lang.reflect.Array.set(arr, 0, family);
                for (String name : new String[]{"sans-serif", "sans-serif-medium", "roboto"}) {
                    try {
                        Method m = Typeface.class.getDeclaredMethod(
                                "createFromFamilies", String.class, arr.getClass());
                        m.setAccessible(true);
                        Object tf = m.invoke(null, name, arr);
                        System.out.println("SEED createFromFamilies(\"" + name + "\", "
                                + path + ") -> " + tf);
                        if (tf instanceof Typeface) return (Typeface) tf;
                    } catch (Throwable t) {
                        System.out.println("SEED createFromFamilies(" + name + ") -> " + unwrap(t));
                    }
                }
            } catch (Throwable t) {
                System.out.println("SEED font family from " + path + " -> " + unwrap(t));
            }
        }
        return null;
    }

    /** Diagnostics: what did SystemFonts read out of /system/etc/fonts.xml? */
    static void diagFonts() {
        try {
            Class<?> sf = Class.forName("android.graphics.fonts.SystemFonts");
            Method m = null;
            for (Method c : sf.getDeclaredMethods()) {
                if (c.getName().equals("getSystemFontConfig") && c.getParameterCount() == 0) m = c;
            }
            if (m == null) m = sf.getDeclaredMethod("getSystemPreinstalledFontConfig");
            m.setAccessible(true);
            Object fc = m.invoke(null);
            Method gf = fc.getClass().getMethod("getFontFamilies");
            java.util.List<?> fams = (java.util.List<?>) gf.invoke(fc);
            System.out.println("DIAG font families=" + fams.size());
            int i = 0;
            for (Object fam : fams) {
                java.util.List<?> fonts = (java.util.List<?>) fam.getClass().getMethod("getFonts").invoke(fam);
                StringBuilder sb = new StringBuilder();
                for (Object f : fonts) {
                    Object fp = f.getClass().getMethod("getFile").invoke(f);
                    sb.append(fp).append(' ');
                }
                System.out.println("DIAG fam[" + (i++) + "] fonts=" + fonts.size() + " " + sb);
                if (i > 4) break;
            }
        } catch (Throwable t) {
            note("[diag] SystemFonts -> " + unwrap(t));
        }
        try {
            Class<?> tf = Class.forName("android.graphics.Typeface");
            for (Field f : tf.getDeclaredFields()) {
                if (!f.getName().startsWith("s")) continue;
                try {
                    f.setAccessible(true);
                    Object v = f.get(null);
                    String sv = v instanceof java.util.Map
                            ? ("map size=" + ((java.util.Map<?, ?>) v).size() + " " + v) : String.valueOf(v);
                    if (sv.length() > 300) sv = sv.substring(0, 300) + "...";
                    System.out.println("DIAG Typeface#" + f.getName() + " = " + sv);
                } catch (Throwable t) { }
            }
        } catch (Throwable t) { note("[diag] Typeface fields -> " + t); }
        try {
            Object font = Class.forName("android.graphics.fonts.Font$Builder")
                    .getConstructor(java.io.File.class).newInstance(new java.io.File("/system/fonts/Roboto-Regular.ttf"));
            Class<?> fb = Class.forName("android.graphics.fonts.Font$Builder");
            Object f = fb.getMethod("build").invoke(font);
            System.out.println("DIAG Font.Builder.build() -> " + f);
            Class<?> ff = Class.forName("android.graphics.fonts.FontFamily$Builder");
            Object ffam = ff.getConstructor(Class.forName("android.graphics.fonts.Font")).newInstance(f);
            Object family = ff.getMethod("build").invoke(ffam);
            System.out.println("DIAG FontFamily.build() -> " + family);
        } catch (Throwable t) {
            note("[diag] Font.Builder -> " + unwrap(t));
        }
    }

    static void stepDraw() {
        if (sSystemContext == null) {
            bad("[draw] no SystemContext -- ActivityThread rung did not pass", new IllegalStateException("blocked"));
            return;
        }
        if (sRes == null) {
            bad("[draw] no Resources -- resources rung did not pass", new IllegalStateException("blocked"));
            return;
        }
        installStubWindowManager();
        diagFonts();
        stepText();
        Context ctx = newContext();
        View content;
        try {
            LayoutInflater inf = LayoutInflater.from(ctx);
            long t0 = System.nanoTime();
            content = inf.inflate(sLayoutId, null, false);
            long us = (System.nanoTime() - t0) / 1000;
            ok("[inflate] LayoutInflater.inflate(0x" + Integer.toHexString(sLayoutId) + ") -> "
               + content.getClass().getName() + " in " + us + " us");
        } catch (Throwable t) {
            bad("[inflate] LayoutInflater.inflate(layout/main)", unwrap(t));
            return;
        }
        try {
            FrameLayout root = new FrameLayout(ctx);
            root.addView(content, new FrameLayout.LayoutParams(
                    ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.MATCH_PARENT));
            long t0 = System.nanoTime();
            root.measure(View.MeasureSpec.makeMeasureSpec(W, View.MeasureSpec.EXACTLY),
                         View.MeasureSpec.makeMeasureSpec(CONTENT_H, View.MeasureSpec.EXACTLY));
            root.layout(0, 0, W, CONTENT_H);
            long us = (System.nanoTime() - t0) / 1000;
            ok("[layout] measure+layout " + W + "x" + CONTENT_H + " @density " + DENSITY
               + " in " + us + " us -> measured " + root.getMeasuredWidth() + "x"
               + root.getMeasuredHeight());
            String tree = dumpTree(root);
            File outDir = new File(System.getProperty(OUT_PROP, "/data/out"));
            outDir.mkdirs();
            write(new File(outDir, "e4h-tree.txt"), tree);
            System.out.println("TREE\n" + tree);

            int bg = windowBackgroundColor();
            Bitmap bm = Bitmap.createBitmap(W, CONTENT_H, Bitmap.Config.ARGB_8888);
            bm.eraseColor(bg);
            Canvas canvas = new Canvas(bm);
            String mode = System.getProperty("q3.mode", "downgrade");
            note("[q3] mode=" + mode + " isHwAccel=" + canvas.isHardwareAccelerated());
            if (mode.contains("nolooper")) {
                // Stock "no Looper on the drawing thread" condition:
                // RippleDrawable.startBackgroundAnimation() self-skips when
                // Looper.myLooper() == null.
                java.lang.reflect.Field lf = android.os.Looper.class.getDeclaredField("sThreadLocal");
                lf.setAccessible(true);
                ((ThreadLocal<?>) lf.get(null)).remove();
                note("[q3] Looper.myLooper() -> " + android.os.Looper.myLooper());
            }
            if (mode.contains("pressed")) {
                View inc = findViewById(root, sRes.getIdentifier("inc", "id", sPkg));
                View reset = findViewById(root, sRes.getIdentifier("reset", "id", sPkg));
                if (inc != null) {
                    // The Button's default StateListAnimator (elevation) is a
                    // *separate* Looper dependency (StateListAnimator/AnimatorSet,
                    // already known from SessionMain). Drop it so this probe
                    // isolates the RippleDrawable.
                    inc.setStateListAnimator(null);
                    inc.setPressed(true);
                    note("[q3] inc pressed=true bg=" + inc.getBackground().getClass().getName()
                         + " drawableState=" + java.util.Arrays.toString(inc.getDrawableState()));
                }
            }
            if (mode.contains("focused")) {
                View reset = findViewById(root, sRes.getIdentifier("reset", "id", sPkg));
                if (reset != null) {
                    android.graphics.drawable.Drawable rb = reset.getBackground();
                    rb.setState(new int[]{android.R.attr.state_focused});
                    note("[q3] reset bg=" + rb.getClass().getName()
                         + " forced state_focused on the drawable");
                }
            }
            boolean demo = mode.contains("demo");
            if (demo) {
                // The recommended product policy, exercised end to end: a pressed
                // Button (real View.setPressed path) and a focused Button, drawn on
                // a Looper-less thread with every RippleDrawable replaced by its
                // content layer for the duration of the frame.
                View inc = findViewById(root, sRes.getIdentifier("inc", "id", sPkg));
                View reset = findViewById(root, sRes.getIdentifier("reset", "id", sPkg));
                for (View v : new View[]{inc, reset}) {
                    if (v != null) v.setStateListAnimator(null);
                }
                if (inc != null) {
                    inc.setPressed(true);
                    note("[q3/demo] inc pressed=" + inc.isPressed()
                         + " drawableState=" + java.util.Arrays.toString(inc.getDrawableState()));
                }
                boolean focused = false;
                if (reset != null) {
                    reset.setFocusable(true);
                    reset.setFocusableInTouchMode(true);
                    try {
                        focused = reset.requestFocus();
                    } catch (Throwable t) {
                        note("[q3/demo] requestFocus -> " + unwrap(t));
                    }
                    if (!focused) {
                        Drawable rb = reset.getBackground();
                        rb.setState(new int[]{android.R.attr.state_enabled,
                                android.R.attr.state_focused, android.R.attr.state_window_focused});
                    }
                    note("[q3/demo] reset focused=" + reset.isFocused() + " (View.requestFocus="
                         + focused + ") drawableState="
                         + java.util.Arrays.toString(reset.getDrawableState()));
                }
                // the tree as it will be drawn: states applied
                String stateTree = dumpTree(root);
                write(new File(System.getProperty(OUT_PROP, "/data/out"), "q3-demo-tree.txt"), stateTree);
                System.out.println("TREE-STATE\n" + stateTree);
            }
            if (mode.equals("replay")) {
                RenderNode node = new RenderNode("q3-replay");
                node.setPosition(0, 0, 200, 200);
                RecordingCanvas rc = node.beginRecording(200, 200);
                android.graphics.Paint rp = new android.graphics.Paint();
                rp.setColor(0xFFFF0000);
                rp.setStyle(android.graphics.Paint.Style.FILL);
                rc.drawCircle(100, 100, 80, rp);
                node.endRecording();
                note("[q3] recorded node hasDisplayList=" + node.hasDisplayList()
                     + " -> bitmapCanvas.drawRenderNode(node)");
                canvas.drawRenderNode(node);
                note("[q3] drawRenderNode returned (replay works on a software canvas)");
                int[] rp2 = new int[W * CONTENT_H];
                bm.getPixels(rp2, 0, W, 0, 0, W, CONTENT_H);
                int red = 0;
                for (int q : rp2) if (q == 0xFFFF0000) red++;
                ok("[q3/replay] red pixels drawn from the RenderNode: " + red);
            }
            String wait = System.getProperty("q3.wait", null);
            if (wait != null) {
                System.out.println("WAIT pid=" + android.os.Process.myPid() + " sleeping " + wait
                        + " ms before draw (attach gdb now)");
                System.out.flush();
                try { Thread.sleep(Long.parseLong(wait)); } catch (InterruptedException ignored) { }
                System.out.println("WAIT resuming");
                System.out.flush();
            }
            long d0 = System.nanoTime();
            // SOFTWARE DOWNGRADE (measured): drawing a RippleDrawable on a
            // non-hardware Canvas kills the process with a silent SIGSEGV inside
            // libhwui (no tombstone, no log) -- it is the only background type in
            // this tree that does. The ripple is a *pressed-state animation*; its
            // static content layer is what the screenshot shows, so we swap in
            // RippleDrawable.getDrawable(0) before drawing and say so in the tree.
            boolean policy = mode.equals("downgrade") || mode.contains("demo");
            int downgraded = policy ? downgradeRipples(root) : 0;
            if (downgraded > 0) {
                note("[draw] software downgrade: " + downgraded
                     + " RippleDrawable(s) -> content layer (ripple needs a hardware canvas)");
            }
            root.draw(canvas);
            long cold_us = (System.nanoTime() - d0) / 1000;
            d0 = System.nanoTime();
            root.draw(canvas);   // warm
            long warm_us = (System.nanoTime() - d0) / 1000;
            ok("[draw] View.draw(Canvas(Bitmap " + W + "x" + CONTENT_H + ")) bg=#"
               + Integer.toHexString(bg) + " cold=" + cold_us + " us warm=" + warm_us + " us");

            File png = new File(outDir, mode.contains("demo") ? "q3-demo-pressed-focused.png"
                                                              : "e4h-view.png");
            FileOutputStream fos = new FileOutputStream(png);
            boolean compressed = bm.compress(Bitmap.CompressFormat.PNG, 100, fos);
            fos.close();
            if (compressed) {
                ok("[png] " + png + " (" + png.length() + " bytes)");
            } else {
                note("[png] Bitmap.compress(PNG) refused; dumping raw pixels instead");
                dumpRaw(bm, new File(outDir, "e4h-view.rgba"));
            }
            // --- variant: same tree with textAllCaps off ----------------------
            // The one *structural* divergence left is that our theme resolves
            // Widget.Material.Button's textAllCaps=true while the device renders
            // sentence case ("Increment"). Rendering both isolates the cause.
            try {
                int n = setAllCaps(root, false);
                root.measure(View.MeasureSpec.makeMeasureSpec(W, View.MeasureSpec.EXACTLY),
                             View.MeasureSpec.makeMeasureSpec(CONTENT_H, View.MeasureSpec.EXACTLY));
                root.layout(0, 0, W, CONTENT_H);
                write(new File(outDir, "e4h-tree-nocaps.txt"), dumpTree(root));
                Bitmap bm2 = Bitmap.createBitmap(W, CONTENT_H, Bitmap.Config.ARGB_8888);
                bm2.eraseColor(bg);
                Canvas c2 = new Canvas(bm2);
                root.draw(c2);
                File png2 = new File(outDir, "e4h-view-nocaps.png");
                FileOutputStream f2 = new FileOutputStream(png2);
                boolean ok2 = bm2.compress(Bitmap.CompressFormat.PNG, 100, f2);
                f2.close();
                ok("[variant] textAllCaps=false on " + n + " TextViews -> re-measured "
                   + content.getMeasuredWidth() + "x" + content.getMeasuredHeight()
                   + " png=" + (ok2 ? png2.getName() : "raw") + " inc="
                   + findViewById(root, sRes.getIdentifier("inc", "id", sPkg)).getWidth());
                bm2.recycle();
            } catch (Throwable t) {
                note("[variant] nocaps -> " + unwrap(t));
            }

            // Non-trivial pixel check: how many pixels differ from the background.
            int[] px = new int[W * CONTENT_H];
            bm.getPixels(px, 0, W, 0, 0, W, CONTENT_H);
            int nonbg = 0;
            for (int p : px) if (p != bg) nonbg++;
            ok("[pixels] differing from background: " + nonbg + " / " + px.length
               + " (" + (100.0 * nonbg / px.length) + "%)");
            bm.recycle();
        } catch (Throwable t) {
            bad("[layout/draw] measure+layout+draw", unwrap(t));
        }
    }

    /**
     * Replace every RippleDrawable in the tree by its content layer. A ripple on
     * a software Canvas is the one drawable that cannot be rasterised here (see
     * the note in stepDraw), and its content layer is the static appearance.
     */
    static int setAllCaps(View v, boolean caps) {
        int n = 0;
        if (v instanceof TextView) {
            ((TextView) v).setAllCaps(caps);
            n++;
        }
        if (v instanceof ViewGroup) {
            ViewGroup g = (ViewGroup) v;
            for (int i = 0; i < g.getChildCount(); i++) n += setAllCaps(g.getChildAt(i), caps);
        }
        return n;
    }

    static View findViewById(View v, int id) {
        if (v.getId() == id) return v;
        if (v instanceof ViewGroup) {
            ViewGroup g = (ViewGroup) v;
            for (int i = 0; i < g.getChildCount(); i++) {
                View r = findViewById(g.getChildAt(i), id);
                if (r != null) return r;
            }
        }
        return null;
    }

    static int downgradeRipples(View v) {
        int n = 0;
        Drawable bg = v.getBackground();
        if (bg instanceof android.graphics.drawable.RippleDrawable) {
            Drawable content = ((android.graphics.drawable.RippleDrawable) bg).getDrawable(0);
            if (content != null) {
                content.setBounds(bg.getBounds());
                v.setBackground(content);
                System.out.println("DOWNGRADE ripple " + v.getClass().getSimpleName()
                        + " -> " + content.getClass().getName());
                n++;
            }
        }
        if (v instanceof ViewGroup) {
            ViewGroup g = (ViewGroup) v;
            for (int i = 0; i < g.getChildCount(); i++) n += downgradeRipples(g.getChildAt(i));
        }
        return n;
    }

    /** Flatten a Drawable stack (StateList->ripple->9patch->layers) for probing. */
    static void collect(Drawable d, java.util.List<Drawable> out, int depth) {
        if (d == null || depth > 4) return;
        out.add(d);
        String cn = d.getClass().getName();
        System.out.println("DRAW   layer depth=" + depth + " " + cn);
        if (d instanceof android.graphics.drawable.LayerDrawable) {
            android.graphics.drawable.LayerDrawable ld = (android.graphics.drawable.LayerDrawable) d;
            for (int i = 0; i < ld.getNumberOfLayers(); i++) collect(ld.getDrawable(i), out, depth + 1);
        } else if (d instanceof android.graphics.drawable.StateListDrawable) {
            android.graphics.drawable.StateListDrawable sl =
                    (android.graphics.drawable.StateListDrawable) d;
            // no public accessor for the children; select the current one instead
            Drawable cur = sl.getCurrent();
            if (cur != null && cur != d) collect(cur, out, depth + 1);
        } else if (d instanceof android.graphics.drawable.RippleDrawable) {
            android.graphics.drawable.RippleDrawable rd = (android.graphics.drawable.RippleDrawable) d;
            collect(rd.getDrawable(0), out, depth + 1);
        } else if (d instanceof android.graphics.drawable.InsetDrawable) {
            collect(((android.graphics.drawable.InsetDrawable) d).getDrawable(), out, depth + 1);
        }
    }

    static int windowBackgroundColor() {
        try {
            android.content.res.TypedArray a = sTheme.obtainStyledAttributes(
                    new int[]{android.R.attr.windowBackground});
            Drawable d = a.getDrawable(0);
            a.recycle();
            if (d instanceof ColorDrawable) return ((ColorDrawable) d).getColor();
            note("windowBackground -> " + d);
        } catch (Throwable t) {
            note("windowBackground lookup -> " + t);
        }
        return Color.WHITE;
    }

    // ------------------------------------------------------------------ tree
    static String dumpTree(View v) {
        StringBuilder sb = new StringBuilder();
        sb.append("{\"bounds\":\"").append(bounds(v)).append("\",");
        sb.append("\"nodes\":[");
        dumpNode(v, sb, new int[]{0});
        sb.append("]}");
        return sb.toString();
    }

    static void dumpNode(View v, StringBuilder sb, int[] n) {
        n[0]++;
        sb.append("{\"class\":\"").append(v.getClass().getName()).append("\"");
        sb.append(",\"id\":\"").append(idName(v)).append("\"");
        String text = v instanceof TextView ? String.valueOf(((TextView) v).getText()) : "";
        sb.append(",\"text\":\"").append(esc(text)).append("\"");
        sb.append(",\"bounds\":\"").append(bounds(v)).append("\"");
        sb.append(",\"measured\":\"").append(v.getMeasuredWidth()).append('x')
          .append(v.getMeasuredHeight()).append("\"");
        sb.append(",\"textSize\":").append(v instanceof TextView ? ((TextView) v).getTextSize() : 0);
        sb.append(",\"clickable\":").append(v.isClickable());
        sb.append(",\"pressed\":").append(v.isPressed());
        sb.append(",\"focused\":").append(v.isFocused());
        sb.append(",\"visibility\":").append(v.getVisibility());
        sb.append(",\"padding\":\"").append(v.getPaddingLeft()).append(',')
          .append(v.getPaddingTop()).append(',').append(v.getPaddingRight()).append(',')
          .append(v.getPaddingBottom()).append('"');
        if (v instanceof TextView) {
            TextView tv = (TextView) v;
            TextPaint tp = tv.getPaint();
            sb.append(",\"textWidth\":").append(tp.measureText(String.valueOf(tv.getText())));
            sb.append(",\"fontMetrics\":\"").append(tp.getFontMetricsInt().top).append(',')
              .append(tp.getFontMetricsInt().ascent).append(',')
              .append(tp.getFontMetricsInt().descent).append(',')
              .append(tp.getFontMetricsInt().bottom).append('"');
        }
        if (v instanceof ViewGroup) {
            ViewGroup g = (ViewGroup) v;
            sb.append(",\"children\":[");
            for (int i = 0; i < g.getChildCount(); i++) {
                if (i > 0) sb.append(',');
                dumpNode(g.getChildAt(i), sb, n);
            }
            sb.append(']');
        }
        sb.append('}');
    }

    static String bounds(View v) {
        int[] xy = new int[2];
        v.getLocationInWindow(xy);
        // getLocationInWindow needs a WindowRecomposer/window; fall back to a
        // walk up the parents so the numbers are the layout coordinates.
        int x = 0, y = 0;
        View c = v;
        while (c != null) {
            x += c.getLeft();
            y += c.getTop();
            if (!(c.getParent() instanceof View)) break;
            c = (View) c.getParent();
        }
        return "[" + x + "," + y + "][" + (x + v.getWidth()) + "," + (y + v.getHeight()) + "]";
    }

    static String idName(View v) {
        try {
            int id = v.getId();
            if (id == View.NO_ID || id == 0) return "";
            return sRes.getResourceName(id);
        } catch (Throwable t) {
            return "";
        }
    }

    static String esc(String s) {
        return s.replace("\\", "\\\\").replace("\"", "\\\"");
    }

    static void write(File f, String s) {
        try (FileOutputStream o = new FileOutputStream(f)) {
            o.write(s.getBytes("UTF-8"));
        } catch (Throwable t) {
            note("write " + f + " -> " + t);
        }
    }

    static void dumpRaw(Bitmap bm, File f) {
        try (FileOutputStream o = new FileOutputStream(f)) {
            java.io.DataOutputStream d = new java.io.DataOutputStream(o);
            d.writeInt(bm.getWidth());
            d.writeInt(bm.getHeight());
            int[] px = new int[bm.getWidth() * bm.getHeight()];
            bm.getPixels(px, 0, bm.getWidth(), 0, 0, bm.getWidth(), bm.getHeight());
            for (int p : px) d.writeInt(p);
            d.flush();
        } catch (Throwable t) {
            note("dumpRaw -> " + t);
        }
    }

    static long peakRssKb() {
        try {
            for (String line : java.nio.file.Files.readAllLines(
                    java.nio.file.Paths.get("/proc/self/status"))) {
                if (line.startsWith("VmHWM:")) {
                    return Long.parseLong(line.replaceAll("[^0-9]", ""));
                }
            }
        } catch (Throwable ignored) {
        }
        return -1;
    }

    static Throwable unwrap(Throwable t) {
        while (t instanceof InvocationTargetException && t.getCause() != null) t = t.getCause();
        return t;
    }
}
