package dev.acos.art;

import android.content.Context;
import android.content.res.AssetManager;
import android.content.res.Configuration;
import android.content.res.Resources;
import android.content.res.XmlResourceParser;
import android.graphics.Bitmap;
import android.graphics.Canvas;
import android.graphics.Color;
import android.graphics.Typeface;
import android.graphics.drawable.ColorDrawable;
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
 * `acos-art render-app` guest: inflate an APK's launcher layout, measure/layout
 * it at a device-accurate size and draw one frame into a software Bitmap, then
 * write {@code frame.png} + {@code tree.json}.
 *
 * Ported from the E4H feasibility probe (results/art-feasibility/E4H.md,
 * research/art-feasibility/h-view/java/dev/acos/e4h/Main.java): same seam, same
 * workarounds, no probes. Everything the outside world needs arrives through
 * the {@code ACOSAPP <json>} line on stdout; failures also print
 * {@code ACOSAPP {"outcome":"crash",...}} with a stable code and the full
 * cause chain.
 *
 * argv: apk pkg outDir width contentHeight densityDpi
 */
public final class AppMain {

    // device-accurate panel: 1080x2400 @ 420 dpi (all ACOS fixtures' oracle)
    static final int PANEL_H = 2400;

    static String sApk, sPkg, sOut;
    static int W = 1080, CONTENT_H = 2062, DENSITY = 420;

    static final List<String> notes = new ArrayList<>();
    static final List<String> warnings = new ArrayList<>();

    public static void main(String[] args) {
        long t0 = System.currentTimeMillis();
        if (args.length < 3) {
            emit("crash", "BAD_ARGS", "need: apk pkg outDir [width] [contentHeight] [density]", t0);
            System.exit(2);
        }
        sApk = args[0];
        sPkg = args[1];
        sOut = args[2];
        if (args.length > 3) W = Integer.parseInt(args[3]);
        if (args.length > 4) CONTENT_H = Integer.parseInt(args[4]);
        if (args.length > 5) DENSITY = Integer.parseInt(args[5]);
        try {
            new File(sOut).mkdirs();
            run();
        } catch (Throwable t) {
            Throwable u = unwrap(t);
            dumpChain(u);
            emit("crash", codeFor(u), String.valueOf(u), t0);
            System.exit(1);
        }
    }

    // ------------------------------------------------------------------ rungs

    static void run() throws Throwable {
        long t0 = System.currentTimeMillis();
        stepGfx();
        stepRuntime();
        ensureActivity();
        stepResources();
        installStubWindowManager();
        seedTypeface();
        Context ctx = newContext();
        View content = inflate(ctx);
        if (!"theme".equals(System.getProperty("acosart.allcaps", "device"))) {
            int n = setAllCaps(content, false);
            if (n > 0) {
                note("textAllCaps=false on " + n + " TextView(s) (device renders sentence case; "
                     + "our theme resolves Widget.Material.Button.textAllCaps=true -- E4H \u00a77)");
            }
        }
        FrameLayout root = layout(ctx, content);
        String tree = dumpTree(root);
        write(new File(sOut, "tree.json"), tree);
        int[] drawUs = draw(root);
        emit("ok", null, null, t0, root, tree, drawUs);
    }

    static void stepGfx() throws Throwable {
        RenderNat.load();
        int ok = 0, miss = 0;
        String[] graphics = {"register_android_graphics_classes"};
        for (String n : graphics) {
            int rc = RenderNat.register(n);
            if (rc == 1) ok++; else { miss++; note("register(" + n + ") -> " + rc); }
        }
        // prove the natives are live with the exact call that blocked E2D's
        // ActivityThread.systemMain() (HardwareRenderer.nSetIsSystemOrPersistent)
        Class<?> hr = Class.forName("android.graphics.HardwareRenderer");
        Method m = null;
        for (Method cand : hr.getDeclaredMethods()) {
            if (cand.getName().equals("nSetIsSystemOrPersistent")) { m = cand; break; }
        }
        if (m == null) throw new NoSuchMethodException("HardwareRenderer.nSetIsSystemOrPersistent");
        m.setAccessible(true);
        m.invoke(null, Boolean.FALSE);
        note("gfx register ok=" + ok + " missing=" + miss);
    }

    static void stepRuntime() throws Throwable {
        String[] runtime = {
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
                "register_android_view_DisplayEventReceiver",
                "register_android_text_AndroidCharacter",
                "register_android_text_Hyphenator",
                "register_android_util_CharsetUtils",
                // NOT registered on purpose: view_SurfaceControl/view_Surface/
                // graphics_{GraphicBuffer,BLASTBufferQueue,SurfaceTexture}. Their
                // register function runs the class initialiser, whose <clinit>
                // calls a native of the same class that is not registered yet =>
                // sticky NoClassDefFoundError. The software draw path needs no
                // Surface (E4H §3.1).
        };
        int ok = 0, miss = 0;
        for (String n : runtime) {
            int rc;
            try {
                rc = RenderNat.register(n);
            } catch (Throwable t) {
                rc = -1;
                note("register(" + n + ") threw " + unwrap(t));
            }
            if (rc == 1) ok++; else { miss++; note("register(" + n + ") -> " + rc); }
        }
        // missing DisplayEventReceiver etc. is tolerated: a static first frame
        // does not run the choreographer. Recorded, not fatal.
        if (miss > 0) warnings.add("runtime_natives_missing=" + miss);
        note("runtime register ok=" + ok + " missing=" + miss);
    }

    static Object sActivityThread;
    static Context sSystemContext;

    static void ensureActivity() throws Throwable {
        android.os.Looper.prepareMainLooper();
        Class<?> at = Class.forName("android.app.ActivityThread");
        Method systemMain = at.getDeclaredMethod("systemMain");
        systemMain.setAccessible(true);
        sActivityThread = systemMain.invoke(null);
        Method getSystemContext = at.getDeclaredMethod("getSystemContext");
        getSystemContext.setAccessible(true);
        sSystemContext = (Context) getSystemContext.invoke(sActivityThread);
        if (sSystemContext == null) throw new IllegalStateException("ActivityThread.getSystemContext() == null");
        note("ActivityThread.systemMain() -> " + sSystemContext);
    }

    static Resources sRes;
    static Resources.Theme sTheme;
    static int sLayoutId;

    static void stepResources() throws Throwable {
        Constructor<AssetManager> c = AssetManager.class.getDeclaredConstructor();
        c.setAccessible(true);
        AssetManager am = c.newInstance();
        Method addAssetPath = AssetManager.class.getDeclaredMethod("addAssetPath", String.class);
        addAssetPath.setAccessible(true);
        addAssetPath.invoke(am, "/system/framework/framework-res.apk");
        addAssetPath.invoke(am, sApk);

        DisplayMetrics dm = new DisplayMetrics();
        dm.widthPixels = W;
        dm.heightPixels = PANEL_H;
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
        // The 3-arg ctor recomputes DisplayMetrics from the Configuration (0x0);
        // push the real panel metrics back, as ResourcesManager does.
        Method uc = Resources.class.getDeclaredMethod(
                "updateConfiguration", Configuration.class, DisplayMetrics.class);
        uc.setAccessible(true);
        uc.invoke(sRes, cfg, dm);

        sLayoutId = sRes.getIdentifier("main", "layout", sPkg);
        if (sLayoutId == 0) throw new IllegalStateException("no layout/main in " + sApk + " (" + sPkg + ")");
        XmlResourceParser xp = sRes.getLayout(sLayoutId);
        String rootTag = null;
        for (int ev = xp.next(); ev != XmlResourceParser.END_DOCUMENT; ev = xp.next()) {
            if (ev == XmlResourceParser.START_TAG) { rootTag = xp.getName(); break; }
        }
        note("Resources " + sRes.getDisplayMetrics().widthPixels + "x"
             + sRes.getDisplayMetrics().heightPixels + " density=" + sRes.getDisplayMetrics().density
             + " layout/main=0x" + Integer.toHexString(sLayoutId) + " root=<" + rootTag + ">");

        sTheme = sRes.newTheme();
        sTheme.applyStyle(android.R.style.Theme_DeviceDefault_Light, true);
    }

    /**
     * The one place the missing binder peer bites: View's constructor asks
     * ViewConfiguration, which asks IWindowManager.hasNavigationBar(displayId)
     * through ServiceManager. Install a reflective Proxy as
     * WindowManagerGlobal.sWindowManagerService answering defaults (false/0/null).
     */
    static void installStubWindowManager() {
        try {
            Class<?> iwm = Class.forName("android.view.IWindowManager");
            Object proxy = java.lang.reflect.Proxy.newProxyInstance(
                    iwm.getClassLoader(), new Class<?>[]{iwm},
                    (p, m, a) -> {
                        note("stub IWindowManager." + m.getName());
                        Class<?> r = m.getReturnType();
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
        } catch (Throwable t) {
            warnings.add("iwm_stub_failed=" + unwrap(t));
        }
    }

    // ------------------------------------------------------------------ text

    /**
     * The system font map is a **zygote artefact**: on device it arrives via
     * Typeface.sSystemFontMapSharedMemory, so a plain dalvikvm64 gets
     * sSystemFontMap == {} / sDefaultTypeface == null and TextView's
     * constructor dies in Typeface.create(null, 0). Rebuild the default family
     * from /system/fonts and install it with the @hide
     * {@code Typeface.setDefault(Typeface)}, which sets the Java field *and*
     * libhwui's gDefaultTypeface (writing the field alone trips libhwui's
     * {@code ASSERT src == nullptr && gDefaultTypeface == nullptr} on the first
     * measureText). See E4H §3.3.
     */
    static void seedTypeface() throws Throwable {
        Class<?> tf = Class.forName("android.graphics.Typeface");
        Field sdef = tf.getDeclaredField("sDefaultTypeface");
        sdef.setAccessible(true);
        if (sdef.get(null) != null) {
            note("Typeface.sDefaultTypeface already set: " + sdef.get(null));
            return;
        }
        Typeface sans = null;
        String seed = null;
        for (String path : new String[]{"/system/fonts/Roboto-Regular.ttf",
                "/system/fonts/RobotoStatic-Regular.ttf"}) {
            if (!new File(path).isFile()) { note("no font at " + path); continue; }
            try {
                Class<?> fb = Class.forName("android.graphics.fonts.Font$Builder");
                Object fontB = fb.getConstructor(File.class).newInstance(new File(path));
                Object font = fb.getMethod("build").invoke(fontB);
                Class<?> fontCls = Class.forName("android.graphics.fonts.Font");
                Class<?> ffamB = Class.forName("android.graphics.fonts.FontFamily$Builder");
                Object familyB = ffamB.getConstructor(fontCls).newInstance(font);
                Class<?> famCls = Class.forName("android.graphics.fonts.FontFamily");
                Object family = ffamB.getMethod("build").invoke(familyB);
                Object arr = java.lang.reflect.Array.newInstance(famCls, 1);
                java.lang.reflect.Array.set(arr, 0, family);
                Method m = Typeface.class.getDeclaredMethod(
                        "createFromFamilies", String.class, arr.getClass());
                m.setAccessible(true);
                Object made = m.invoke(null, "sans-serif", arr);
                if (made instanceof Typeface) { sans = (Typeface) made; seed = path; break; }
            } catch (Throwable t) {
                note("font family from " + path + " -> " + unwrap(t));
            }
        }
        if (sans == null) throw new IllegalStateException("no usable font in /system/fonts");
        boolean installed = false;
        for (Method cand : Typeface.class.getDeclaredMethods()) {
            if (!cand.getName().equals("setDefault") || cand.getParameterCount() != 1) continue;
            try {
                cand.setAccessible(true);
                cand.invoke(null, sans);
                installed = true;
            } catch (Throwable t) {
                note("Typeface.setDefault -> " + unwrap(t));
            }
        }
        if (!installed) {
            sdef.set(null, sans);   // Java-only: libhwui still asserts (documented)
            warnings.add("typeface_setDefault_unavailable");
        }
        Field sd = tf.getDeclaredField("sDefaults");
        sd.setAccessible(true);
        Typeface[] defaults = new Typeface[4];
        for (int i = 0; i < 4; i++) defaults[i] = sans;
        sd.set(null, defaults);
        note("Typeface default seeded from " + seed + " -> " + sans);
    }

    // --------------------------------------------------------------- context

    static final class MiniContext extends ContextThemeWrapper {
        private final Resources res;
        private final Resources.Theme th;
        private LayoutInflater inflater;

        MiniContext(Context base, Resources res, Resources.Theme th) {
            super(base, 0);
            this.res = res;
            this.th = th;
        }

        @Override public android.content.pm.PackageManager getPackageManager() {
            return sPackageManager != null ? sPackageManager : super.getPackageManager();
        }
        @Override public Resources getResources() { return res; }
        @Override public AssetManager getAssets() { return res.getAssets(); }
        @Override public Resources.Theme getTheme() { return th; }

        @Override public Object getSystemService(String name) {
            if (Context.LAYOUT_INFLATER_SERVICE.equals(name)) {
                if (inflater == null) inflater = LayoutInflater.from(getBaseContext()).cloneInContext(this);
                return inflater;
            }
            return super.getSystemService(name);
        }
    }

    static Context newContext() {
        installStubPackageManager();
        installStubInputMethodManager();
        return new MiniContext(sSystemContext, sRes, sTheme);
    }

    /**
     * `Editor$ProcessTextIntentActionsHandler` (reached from `Editor.<init>` for
     * any editable TextView, i.e. every EditText) does
     * `Objects.requireNonNull(context.getPackageManager())`. `ActivityThread`
     * caches `sPackageManager` through `AppGlobals.getPackageManager()`, which
     * needs a binder peer, so it is null here and EditText inflation NPEs.
     *
     * A `PackageManager` is an abstract class, so a reflective Proxy will not do:
     * instantiate `ApplicationPackageManager` over a Proxy'd @hide
     * `IPackageManager` that answers defaults. Nothing on the static-frame path
     * queries it (the process-text menu is built when a selection action is
     * requested), it only has to be non-null.
     */
    static void installStubPackageManager() {
        if (sPackageManager != null) return;
        try {
            Class<?> ipm = Class.forName("android.content.pm.IPackageManager");
            Object pm = java.lang.reflect.Proxy.newProxyInstance(
                    ipm.getClassLoader(), new Class<?>[]{ipm}, (p, m, a) -> defaultOf(m.getReturnType()));
            Class<?> apm = Class.forName("android.app.ApplicationPackageManager");
            Class<?> ctxImpl = Class.forName("android.app.ContextImpl");
            Constructor<?> c = apm.getDeclaredConstructor(ctxImpl, ipm);
            c.setAccessible(true);
            sPackageManager = (android.content.pm.PackageManager) c.newInstance(null, pm);
            note("stub PackageManager = " + sPackageManager.getClass().getName());
        } catch (Throwable t) {
            try {
                Class<?> unsafe = Class.forName("sun.misc.Unsafe");
                Field uf = unsafe.getDeclaredField("theUnsafe");
                uf.setAccessible(true);
                Object u = uf.get(null);
                sPackageManager = (android.content.pm.PackageManager) unsafe
                        .getMethod("allocateInstance", Class.class)
                        .invoke(u, Class.forName("android.app.ApplicationPackageManager"));
                warnings.add("package_manager_unsafe_instance");
            } catch (Throwable t2) {
                warnings.add("package_manager_stub_failed=" + unwrap(t2));
            }
        }
    }

    static android.content.pm.PackageManager sPackageManager;

    /**
     * `InputMethodManager.forContext()` (reached from `Editor` as soon as a text
     * view is editable: `applySingleLine` -> `setTransformationMethod` ->
     * `imm.restartInput`) throws
     * `IllegalStateException: IInputMethodManager is not available` when
     * `ServiceManager.getService("input_method")` is null.
     *
     * There is no servicemanager here, but `ServiceManager` keeps a static
     * cache (`sCache`) that `getService()` consults *before* binder. Putting one
     * object there that is both an `IBinder` and an `IInputMethodManager` (its
     * `queryLocalInterface` returns itself, which is exactly what
     * `IInputMethodManager.Stub.asInterface` checks) makes the whole IME path a
     * local no-op instead of a crash: a static first frame never shows an IME.
     */
    static void installStubInputMethodManager() {
        if (sInputMethodManagerStub) return;
        try {
            Class<?> ibinder = Class.forName("android.os.IBinder");
            Class<?> iimm = Class.forName("com.android.internal.view.IInputMethodManager");
            final Object[] self = new Object[1];
            Object proxy = java.lang.reflect.Proxy.newProxyInstance(
                    iimm.getClassLoader(), new Class<?>[]{ibinder, iimm}, (p, m, a) -> {
                        String n = m.getName();
                        if (n.equals("queryLocalInterface")) return self[0];
                        if (n.equals("getInterfaceDescriptor")) return iimm.getName();
                        if (n.equals("isBinderAlive") || n.equals("pingBinder")) return Boolean.TRUE;
                        if (n.equals("toString")) return "ACOS stub IInputMethodManager";
                        if (n.equals("hashCode")) return System.identityHashCode(p);
                        if (n.equals("equals")) return p == (a == null ? null : a[0]);
                        return defaultOf(m.getReturnType());
                    });
            self[0] = proxy;
            Class<?> sm = Class.forName("android.os.ServiceManager");
            Field f = sm.getDeclaredField("sCache");
            f.setAccessible(true);
            Object cache = f.get(null);
            ((java.util.Map) cache).put("input_method", proxy);
            sInputMethodManagerStub = true;
            note("stub ServiceManager sCache[input_method] = local IInputMethodManager");
        } catch (Throwable t) {
            warnings.add("input_method_stub_failed=" + unwrap(t));
        }
    }

    static boolean sInputMethodManagerStub;

    static Object defaultOf(Class<?> r) {
        if (r == boolean.class) return Boolean.FALSE;
        if (r == int.class) return 0;
        if (r == long.class) return 0L;
        if (r == float.class) return 0f;
        if (r == double.class) return 0d;
        if (r == short.class) return (short) 0;
        if (r == byte.class) return (byte) 0;
        if (r == char.class) return (char) 0;
        if (r == java.util.List.class) return new java.util.ArrayList<>();
        if (r.isArray()) return java.lang.reflect.Array.newInstance(r.getComponentType(), 0);
        if (r == String.class) return "";
        return null;
    }

    // ------------------------------------------------------------------ draw

    static View inflate(Context ctx) throws Throwable {
        LayoutInflater inf = LayoutInflater.from(ctx);
        long t0 = System.nanoTime();
        try {
            View v = inf.inflate(sLayoutId, null, false);
            note("inflate " + v.getClass().getName() + " in " + (System.nanoTime() - t0) / 1000 + " us");
            return v;
        } catch (Throwable t) {
            Throwable u = unwrap(t);
            dumpChain(u);
            throw u;
        }
    }

    static int setAllCaps(View v, boolean caps) {
        int n = 0;
        if (v instanceof TextView && ((TextView) v).isAllCaps() != caps) {
            ((TextView) v).setAllCaps(caps);
            n++;
        }
        if (v instanceof ViewGroup) {
            ViewGroup g = (ViewGroup) v;
            for (int i = 0; i < g.getChildCount(); i++) n += setAllCaps(g.getChildAt(i), caps);
        }
        return n;
    }

    static FrameLayout layout(Context ctx, View content) {
        FrameLayout root = new FrameLayout(ctx);
        root.addView(content, new FrameLayout.LayoutParams(
                ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.MATCH_PARENT));
        long t0 = System.nanoTime();
        root.measure(View.MeasureSpec.makeMeasureSpec(W, View.MeasureSpec.EXACTLY),
                     View.MeasureSpec.makeMeasureSpec(CONTENT_H, View.MeasureSpec.EXACTLY));
        root.layout(0, 0, W, CONTENT_H);
        note("measure+layout " + W + "x" + CONTENT_H + " in " + (System.nanoTime() - t0) / 1000 + " us");
        return root;
    }

    static int[] draw(FrameLayout root) throws Throwable {
        int bg = windowBackgroundColor();
        Bitmap bm = Bitmap.createBitmap(W, CONTENT_H, Bitmap.Config.ARGB_8888);
        bm.eraseColor(bg);
        Canvas canvas = new Canvas(bm);
        // SOFTWARE DOWNGRADE (measured, E4H §3.4): RippleDrawable.draw() on a
        // non-hardware Canvas kills the process with a silent SIGSEGV inside
        // libhwui -- no tombstone, no log line. A ripple is a *pressed-state
        // animation*; its static appearance is its content layer, so swap in
        // RippleDrawable.getDrawable(0). Every other drawable in the fixture set
        // draws on a software canvas.
        int downgraded = downgradeRipples(root);
        if (downgraded > 0) note("downgraded " + downgraded + " RippleDrawable -> content layer (software canvas)");
        long t0 = System.nanoTime();
        root.draw(canvas);
        long cold = (System.nanoTime() - t0) / 1000;
        t0 = System.nanoTime();
        root.draw(canvas);
        long warm = (System.nanoTime() - t0) / 1000;
        File png = new File(sOut, "frame.png");
        try (FileOutputStream fos = new FileOutputStream(png)) {
            if (!bm.compress(Bitmap.CompressFormat.PNG, 100, fos)) {
                throw new IllegalStateException("Bitmap.compress(PNG) refused");
            }
        }
        int[] px = new int[W * CONTENT_H];
        bm.getPixels(px, 0, W, 0, 0, W, CONTENT_H);
        int nonbg = 0;
        for (int p : px) if (p != bg) nonbg++;
        note("draw bg=#" + Integer.toHexString(bg) + " cold=" + cold + " us warm=" + warm + " us"
             + " nonbg_px=" + nonbg + " (cache " + root.isDrawingCacheEnabled() + ")");
        bm.recycle();
        return new int[]{(int) cold, (int) warm, nonbg, bg};
    }

    static int downgradeRipples(View v) {
        int n = 0;
        Drawable bg = v.getBackground();
        if (bg instanceof android.graphics.drawable.RippleDrawable) {
            Drawable content = ((android.graphics.drawable.RippleDrawable) bg).getDrawable(0);
            if (content != null) {
                content.setBounds(bg.getBounds());
                v.setBackground(content);
                note("DOWNGRADE ripple " + v.getClass().getSimpleName() + " -> " + content.getClass().getName());
                n++;
            }
        }
        if (v instanceof ViewGroup) {
            ViewGroup g = (ViewGroup) v;
            for (int i = 0; i < g.getChildCount(); i++) n += downgradeRipples(g.getChildAt(i));
        }
        return n;
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
        sb.append("{\"bounds\":\"").append(bounds(v)).append("\",\"nodes\":[");
        dumpNode(v, sb);
        sb.append("]}");
        return sb.toString();
    }

    static void dumpNode(View v, StringBuilder sb) {
        sb.append("{\"class\":\"").append(v.getClass().getName()).append("\"");
        sb.append(",\"id\":\"").append(idName(v)).append("\"");
        String text = v instanceof TextView ? String.valueOf(((TextView) v).getText()) : "";
        sb.append(",\"text\":\"").append(esc(text)).append("\"");
        sb.append(",\"bounds\":\"").append(bounds(v)).append("\"");
        sb.append(",\"measured\":\"").append(v.getMeasuredWidth()).append('x')
          .append(v.getMeasuredHeight()).append("\"");
        sb.append(",\"clickable\":").append(v.isClickable());
        sb.append(",\"visibility\":").append(v.getVisibility());
        sb.append(",\"padding\":\"").append(v.getPaddingLeft()).append(',')
          .append(v.getPaddingTop()).append(',').append(v.getPaddingRight()).append(',')
          .append(v.getPaddingBottom()).append('"');
        if (v instanceof TextView) {
            TextView tv = (TextView) v;
            TextPaint tp = tv.getPaint();
            sb.append(",\"textSize\":").append(tv.getTextSize());
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
                dumpNode(g.getChildAt(i), sb);
            }
            sb.append(']');
        }
        sb.append('}');
    }

    static String bounds(View v) {
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

    // --------------------------------------------------------------- plumbing

    static void note(String s) { notes.add(s); }

    static void emit(String outcome, String code, String message, long t0) {
        emit(outcome, code, message, t0, null, null, null);
    }

    static void emit(String outcome, String code, String message, long t0,
                     FrameLayout root, String tree, int[] drawUs) {
        StringBuilder sb = new StringBuilder();
        sb.append("{\"outcome\":\"").append(outcome).append("\"");
        if (code != null) sb.append(",\"code\":\"").append(esc(code)).append("\"");
        if (message != null) sb.append(",\"message\":\"").append(esc(message)).append("\"");
        sb.append(",\"width\":").append(W).append(",\"contentHeight\":").append(CONTENT_H)
          .append(",\"densityDpi\":").append(DENSITY);
        if (drawUs != null) {
            sb.append(",\"draw_cold_us\":").append(drawUs[0])
              .append(",\"draw_warm_us\":").append(drawUs[1])
              .append(",\"nonbg_px\":").append(drawUs[2])
              .append(",\"background\":\"#").append(Integer.toHexString(drawUs[3])).append("\"");
            sb.append(",\"nodes\":").append(countNodes(tree));
        }
        sb.append(",\"wall_ms\":").append(System.currentTimeMillis() - t0);
        sb.append(",\"peak_rss_kb\":").append(peakRssKb());
        sb.append(",\"warnings\":[");
        for (int i = 0; i < warnings.size(); i++) {
            if (i > 0) sb.append(',');
            sb.append('"').append(esc(warnings.get(i))).append('"');
        }
        sb.append("],\"notes\":[");
        for (int i = 0; i < notes.size(); i++) {
            if (i > 0) sb.append(',');
            sb.append('"').append(esc(notes.get(i))).append('"');
        }
        sb.append("]}");
        System.out.println("ACOSAPP " + sb);
        System.out.flush();
    }

    static int countNodes(String tree) {
        if (tree == null) return 0;
        int n = 0;
        for (int i = tree.indexOf("\"class\":"); i >= 0; i = tree.indexOf("\"class\":", i + 1)) n++;
        return n;
    }

    static String codeFor(Throwable t) {
        String n = t.getClass().getSimpleName();
        String m = String.valueOf(t.getMessage());
        if (n.contains("OutOfMemory")) return "GUEST_OOM";
        if (n.contains("InflateException")) return "INFLATE_FAILED";
        if (n.contains("UnsatisfiedLink")) return "NATIVE_MISSING";
        if (m.contains("no layout/main")) return "NO_LAYOUT";
        if (m.contains("no usable font")) return "NO_FONTS";
        return "GUEST_ERROR";
    }

    static void dumpChain(Throwable t) {
        for (Throwable c = unwrap(t); c != null; c = c.getCause()) {
            System.out.println("CAUSE " + c);
            StackTraceElement[] st = c.getStackTrace();
            for (StackTraceElement e : st) System.out.println("     at " + e);
            if (c.getCause() == c) break;
        }
        System.out.flush();
    }

    static Throwable unwrap(Throwable t) {
        while ((t instanceof InvocationTargetException
                || t.getClass().getName().endsWith("InvocationTargetException"))
                && t.getCause() != null) {
            t = t.getCause();
        }
        return t;
    }

    static String esc(String s) {
        return s.replace("\\", "\\\\").replace("\"", "\\\"").replace("\n", "\\n");
    }

    static void write(File f, String s) {
        try (FileOutputStream o = new FileOutputStream(f)) {
            o.write(s.getBytes("UTF-8"));
        } catch (Throwable t) {
            warnings.add("write_" + f.getName() + "_failed");
        }
    }

    static long peakRssKb() {
        try {
            for (String line : java.nio.file.Files.readAllLines(
                    java.nio.file.Paths.get("/proc/self/status"))) {
                if (line.startsWith("VmHWM:")) return Long.parseLong(line.replaceAll("[^0-9]", ""));
            }
        } catch (Throwable ignored) {
        }
        return -1;
    }
}
