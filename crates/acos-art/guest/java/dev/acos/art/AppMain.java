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
 * argv: apk pkg outDir width contentHeight densityDpi [idmap,idmap,...]
 */
public final class AppMain {

    // device-accurate panel: 1080x2400 @ 420 dpi (all ACOS fixtures' oracle)
    static int PANEL_H = 2400;
    /** The oracle's system-bar insets (statusBarBackground / navigationBarBackground). */
    static int STATUS_INSET = 128, NAV_INSET = 63;

    static String sApk, sPkg, sOut;
    static int W = 1080, CONTENT_H = 2062, DENSITY = 420;
    /** The image's static RRO idmaps, in application order (may be empty). */
    static String[] sIdmaps = new String[0];
    /** The activity's theme resource (0 = the platform default) and label. */
    static int sThemeId;
    static String sTitle = "";
    static int sTargetSdk = 34;
    static android.view.Window sWindow;

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
        if (args.length > 6 && !args[6].isEmpty()) sIdmaps = args[6].split(",");
        // Window geometry + manifest-derived chrome (all data, no constants here).
        if (args.length > 7) PANEL_H = Integer.parseInt(args[7]);
        if (args.length > 8) STATUS_INSET = Integer.parseInt(args[8]);
        if (args.length > 9) NAV_INSET = Integer.parseInt(args[9]);
        if (args.length > 10) sThemeId = parseTheme(args[10]);
        if (args.length > 11) sTitle = args[11];
        if (args.length > 12) sTargetSdk = Integer.parseInt(args[12]);
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
        View decor = buildWindow(ctx);
        layoutWindow(decor);
        String tree = dumpTree(decor);
        write(new File(sOut, "tree.json"), tree);
        int[] drawUs = draw(decor);
        emit("ok", null, null, t0, decor, tree, drawUs);
    }

    static int parseTheme(String s) {
        if (s == null || s.isEmpty() || s.equals("0")) return 0;
        try {
            return (int) Long.parseLong(s.replace("0x", ""), 16);
        } catch (Throwable ignored) {
            return 0;
        }
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
                // Toolbar/RenderNode hold VirtualRefBasePtrs (native strong refs);
                // a standalone class, no SurfaceControl <clinit> involved.
                "register_com_android_internal_util_VirtualRefBasePtr",
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
        AssetManager am = buildAssetManager();

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
        // The activity theme, exactly as ActivityThread applies it: the
        // manifest's android:theme (an app style resource id) when present,
        // otherwise the platform default for a day/light device.
        // Resources.selectDefaultTheme(0, targetSdk >= N) => DeviceDefault.Light
        // with the dark action bar, which is what the API-34 oracle shows.
        int base = sThemeId != 0 ? sThemeId : android.R.style.Theme_DeviceDefault_Light_DarkActionBar;
        sTheme.applyStyle(base, true);
        note("theme=0x" + Integer.toHexString(base) + (sThemeId != 0 ? " (manifest)" : " (platform default)"));
        probeTheme();
    }

    // ------------------------------------------------- the device's Resources

    static final String TARGET_APK = "/system/framework/framework-res.apk";

    /**
     * Build the {@code AssetManager} the way {@code ResourcesManager} does at
     * zygote time: target APK, then one {@code ApkAssets} per applied static
     * overlay (from its idmap), then the app APK, all through
     * {@code AssetManager.Builder}. No ACOS resource code and no hardcoded
     * attribute: the idmaps come from the image's own {@code idmap2} and the
     * overlay values are resolved by stock {@code libandroidfw} (P2c).
     *
     * Order is priority (later {@code addApkAssets} wins). The overlay APKs are
     * loaded *by the idmap*, which names both the target and the overlay path,
     * so they must not be added again.
     *
     * Falls back to the plain {@code AssetManager} constructor +
     * {@code addAssetPath} when {@code ApkAssets}/{@code Builder} are missing
     * (or an idmap is stale), and says so in the notes/warnings — it never
     * silently renders a different resource set.
     */
    static AssetManager buildAssetManager() throws Throwable {
        try {
            AssetManager am = apkAssetsAssetManager();
            return am;
        } catch (Throwable t) {
            Throwable u = unwrap(t);
            warnings.add("apk_assets_unavailable=" + u);
            note("AssetManager via ApkAssets.Builder unavailable (" + u + "); "
                 + "falling back to addAssetPath (NO overlays applied)");
            return legacyAssetManager();
        }
    }

    static AssetManager legacyAssetManager() throws Throwable {
        Constructor<AssetManager> c = AssetManager.class.getDeclaredConstructor();
        c.setAccessible(true);
        AssetManager am = c.newInstance();
        Method addAssetPath = AssetManager.class.getDeclaredMethod("addAssetPath", String.class);
        addAssetPath.setAccessible(true);
        addAssetPath.invoke(am, TARGET_APK);
        addAssetPath.invoke(am, sApk);
        return am;
    }

    static AssetManager apkAssetsAssetManager() throws Throwable {
        Class<?> aa = Class.forName("android.content.res.ApkAssets");
        Method loadFromPath = aa.getMethod("loadFromPath", String.class);
        Object target = loadFromPath.invoke(null, TARGET_APK);

        java.util.List<Object> assets = new java.util.ArrayList<>();
        assets.add(target);
        if (sIdmaps.length > 0) {
            Object[] idmapAssets = new Object[sIdmaps.length];
            int loaded = 0;
            long t0 = System.nanoTime();
            for (String idmap : sIdmaps) {
                try {
                    idmapAssets[loaded] = apkAssetsOverlay(aa, idmap);
                    assets.add(idmapAssets[loaded]);
                    loaded++;
                } catch (Throwable t) {
                    Throwable u = unwrap(t);
                    warnings.add("overlay_idmap_failed=" + idmap + ":" + u);
                    note("idmap " + idmap + " not applied: " + u);
                }
            }
            note("overlays: " + loaded + "/" + sIdmaps.length + " static RRO idmap(s) loaded in "
                 + ((System.nanoTime() - t0) / 1000000.0) + " ms");
            if (loaded == 0) warnings.add("overlays_unavailable=" + sIdmaps.length);
        }
        assets.add(loadFromPath.invoke(null, sApk));

        Class<?> bc = Class.forName("android.content.res.AssetManager$Builder");
        Constructor<?> ctor = bc.getDeclaredConstructor();
        ctor.setAccessible(true);
        Object builder = ctor.newInstance();
        Method add = bc.getMethod("addApkAssets", aa);
        add.setAccessible(true);
        for (Object a : assets) add.invoke(builder, a);
        Method build = bc.getMethod("build");
        build.setAccessible(true);
        AssetManager am = (AssetManager) build.invoke(builder);
        note("AssetManager.Builder: " + assets.size() + " ApkAssets (" + TARGET_APK
             + ", " + (assets.size() - 2) + " overlay(s), " + sApk + ")");
        return am;
    }

    /**
     * {@code ApkAssets.loadOverlayFromPath(idmapPath, loadFlags)} — API 34 takes
     * an {@code int} flag (not a {@code boolean system}). The flag only records
     * provenance; {@code PROPERTY_SYSTEM} (0x1) is what the framework passes.
     */
    static Object apkAssetsOverlay(Class<?> aa, String idmapPath) throws Throwable {
        Method hit = null;
        for (Method m : aa.getDeclaredMethods()) {
            if (!m.getName().equals("loadOverlayFromPath")) continue;
            Class<?>[] p = m.getParameterTypes();
            if (p.length == 2 && p[0] == String.class && p[1] == int.class) { hit = m; break; }
        }
        if (hit == null) throw new NoSuchMethodException("ApkAssets.loadOverlayFromPath(String,int)");
        hit.setAccessible(true);
        int flags = 0x1;
        try {
            Field f = aa.getDeclaredField("PROPERTY_SYSTEM");
            f.setAccessible(true);
            flags = f.getInt(null);
        } catch (Throwable ignored) {
        }
        return hit.invoke(null, idmapPath, flags);
    }

    /**
     * Evidence that the *overlays* (not a guest override) decide the theme:
     * the framework bool the Material button's text appearance reads, plus the
     * overlaid font-family strings.
     */
    static void probeTheme() {
        try {
            int boolId = sRes.getIdentifier("config_buttonTextAllCaps", "bool", "android");
            note("android:bool/config_buttonTextAllCaps=0x" + Integer.toHexString(boolId)
                 + " -> " + (boolId == 0 ? "<none>" : String.valueOf(sRes.getBoolean(boolId)))
                 + " (overlay decides; the guest never forces textAllCaps)");
        } catch (Throwable t) {
            note("config_buttonTextAllCaps probe -> " + t);
        }
        for (String n : new String[]{"config_bodyFontFamily", "config_headlineFontFamily"}) {
            try {
                int id = sRes.getIdentifier(n, "string", "android");
                if (id != 0) note("android:string/" + n + " -> " + sRes.getString(id));
            } catch (Throwable ignored) {
            }
        }
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
        Field fMap = tf.getDeclaredField("sSystemFontMap");
        fMap.setAccessible(true);
        Field fFallback = tf.getDeclaredField("sSystemFallbackMap");
        fFallback.setAccessible(true);

        // 1. What zygote builds: the preinstalled FontConfig -> sSystemFontMap /
        //    sSystemFallbackMap. Both are @hide statics on SystemFonts.
        java.util.Map<String, Typeface> map = new android.util.ArrayMap<>();
        java.util.Map<String, Object> fallback = new android.util.ArrayMap<>();
        // API 34 shut the fallback builder's second parameter: it is the
        // *font-file ByteBuffer cache* (`ArrayMap<String, ByteBuffer>`), not an
        // output map. Passing the output map there used to poison it with
        // path -> DirectByteBuffer entries and made the later
        // `initSystemDefaultTypefaces` cast blow up.
        java.util.Map<String, java.nio.ByteBuffer> fontBuffers = new android.util.ArrayMap<>();
        String source = "none";
        try {
            Class<?> sf = Class.forName("android.graphics.fonts.SystemFonts");
            Class<?> fcc = Class.forName("android.text.FontConfig");
            Method pre = sf.getDeclaredMethod("getSystemPreinstalledFontConfig");
            pre.setAccessible(true);
            Object fc = pre.invoke(null);
            try {
                java.util.List<?> fams = (java.util.List<?>) fc.getClass()
                        .getMethod("getFontFamilies").invoke(fc);
                note("SystemFonts FontConfig families=" + fams.size()
                     + " (font files the map/fallback are built from)");
            } catch (Throwable ignored) {
            }
            // Order matters: on API 34 `buildSystemTypefaces(FontConfig, Map)`
            // takes the **fallback map** (family name -> FontFamily[]) as its
            // second argument and *returns* a new `name -> Typeface` map; it
            // does not fill the argument. Called with an empty map (as P3b
            // did) every alias in `/system/etc/fonts.xml` (`sans-serif-medium`,
            // `sans-serif-light`, `serif`, ...) resolves against nothing, so
            // the returned map -- and therefore `sSystemFontMap` -- is empty
            // and every named family silently collapses to `sDefaults[0]`
            // (Roboto Regular), not to the weight the config asks for.
            try {
                Method bsf = sf.getDeclaredMethod("buildSystemFallback", fcc, android.util.ArrayMap.class);
                bsf.setAccessible(true);
                Object fb = bsf.invoke(null, fc, fontBuffers);
                if (fb instanceof java.util.Map) {
                    @SuppressWarnings("unchecked")
                    java.util.Map<String, Object> named = (java.util.Map<String, Object>) fb;
                    fallback.putAll(named);
                }
            } catch (NoSuchMethodException e) {
                note("SystemFonts.buildSystemFallback(FontConfig,ArrayMap) missing");
            }
            Method bst = sf.getDeclaredMethod("buildSystemTypefaces", fcc, java.util.Map.class);
            bst.setAccessible(true);
            Object built = bst.invoke(null, fc, fallback);
            if (built instanceof java.util.Map) {
                @SuppressWarnings("unchecked")
                java.util.Map<String, Typeface> named = (java.util.Map<String, Typeface>) built;
                map.putAll(named);
            }
            source = "SystemFonts";
        } catch (Throwable t) {
            note("SystemFonts font map -> " + unwrap(t));
        }

        // 2. The default family. If the map has no "sans-serif" (a bare font set
        //    with no named families), build one from the real font files.
        Typeface base = map.get("sans-serif");
        if (base == null) {
            base = typefaceFromFontFile("/system/fonts/Roboto-Regular.ttf");
        }
        if (base == null) {
            base = typefaceFromFontFile("/system/fonts/RobotoStatic-Regular.ttf");
        }
        if (base == null) {
            throw new IllegalStateException(
                    "no usable font in /system/fonts (no system fonts provisioned?)");
        }
        note("SystemFonts.buildSystemTypefaces -> " + map.size() + " named families");
        if (map.isEmpty()) {
            // Never install an empty map: then Typeface.create(name) cannot resolve
            // anything and every themed family would silently collapse to the
            // defaults (the image ships no Google Sans; the *device* does the same,
            // but only for families it lacks, not for all of them).
            map.put("sans-serif", base);
            map.put("roboto", base);
            source = "font-file fallback";
        }

        fMap.set(null, map);
        fFallback.set(null, fallback);

        // 3. The device default, on both sides: Typeface.setDefault() writes the
        //    Java field *and* libhwui's gDefaultTypeface (the field alone trips
        //    libhwui's ASSERT src == nullptr && gDefaultTypeface == nullptr).
        boolean installed = false;
        for (Method cand : Typeface.class.getDeclaredMethods()) {
            if (!cand.getName().equals("setDefault") || cand.getParameterCount() != 1) continue;
            try {
                cand.setAccessible(true);
                cand.invoke(null, base);
                installed = true;
            } catch (Throwable t) {
                note("Typeface.setDefault -> " + unwrap(t));
            }
        }
        if (!installed) {
            sdef.set(null, base);   // Java-only: libhwui still asserts (documented)
            warnings.add("typeface_setDefault_unavailable");
        }

        // 4. sDefaults[0..3] — the third zygote artefact. Typeface.create(name)
        //    with a family the image does not have returns null and falls through
        //    to create(null, style) == sDefaults[style]; the overlaid theme names
        //    google-sans-text, which this image (and the device) does not ship
        //    (P2c §4.2). All four entries must be non-null or inflate NPEs.
        Typeface[] defaults = new Typeface[4];
        defaults[0] = base;
        defaults[1] = orElse(Typeface.create(base, Typeface.BOLD), base);
        defaults[2] = orElse(Typeface.create(base, Typeface.ITALIC), base);
        defaults[3] = orElse(Typeface.create(base, Typeface.BOLD_ITALIC), base);
        Field sd = tf.getDeclaredField("sDefaults");
        sd.setAccessible(true);
        sd.set(null, defaults);
        note("Typeface: sSystemFontMap=" + map.size() + " families (" + source + "), sSystemFallbackMap="
             + fallback.size() + ", sDefaultTypeface=" + base + ", sDefaults=["
             + (defaults[0] != null) + "," + (defaults[1] != null) + ","
             + (defaults[2] != null) + "," + (defaults[3] != null) + "]");
    }

    static Typeface orElse(Typeface a, Typeface b) {
        return a != null ? a : b;
    }

    /** {@code Typeface.createFromFamilies("sans-serif", FontFamily[1])} from a file. */
    static Typeface typefaceFromFontFile(String path) {
        if (!new File(path).isFile()) {
            note("no font at " + path);
            return null;
        }
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
            if (made instanceof Typeface) {
                note("font family from " + path + " -> " + made);
                return (Typeface) made;
            }
        } catch (Throwable t) {
            note("font family from " + path + " -> " + unwrap(t));
        }
        try {
            return Typeface.createFromFile(path);
        } catch (Throwable t) {
            note("Typeface.createFromFile(" + path + ") -> " + unwrap(t));
            return null;
        }
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
        @Override public String getPackageName() { return sPkg; }
        @Override public String getOpPackageName() { return sPkg; }

        /**
         * The *app's* ApplicationInfo, not the system context's. PhoneWindow
         * derives targetSdk from it (pre-L vs modern window flags) and
         * DecorView/Window read packageName/sourceDir.
         */
        @Override public android.content.pm.ApplicationInfo getApplicationInfo() {
            if (ai == null) {
                ai = new android.content.pm.ApplicationInfo();
                ai.packageName = sPkg;
                ai.sourceDir = sApk;
                ai.publicSourceDir = sApk;
                ai.targetSdkVersion = sTargetSdk;
                ai.minSdkVersion = 26;
                ai.uid = android.os.Process.myUid();
            }
            return ai;
        }
        private android.content.pm.ApplicationInfo ai;
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
        installStubSettingsProvider();
        return new MiniContext(sSystemContext, sRes, sTheme);
    }

    /**
     * `PhoneWindow`'s constructor reads a `Settings.Global` int
     * (`DEVELOPMENT_RENDER_SHADOWS_IN_COMPOSITOR`) and `Settings` reaches it
     * through `ContentResolver` -> `IActivityManager.getContentProvider`, which
     * a bare `dalvikvm64` has no peer for. Rather than proxying the whole
     * ActivityManager, register a *local read-only* provider for the `settings`
     * authority in `ActivityThread.mProviderMap`: that is the same map
     * `acquireExistingProvider` consults first, so the framework's own
     * `Settings.NameValueCache` path runs unchanged and unset keys fall through
     * to the caller's default (exactly what a missing settings row means).
     * No app data is invented: unknown keys return an empty answer.
     */
    static void installStubSettingsProvider() {
        try {
            Class<?> at = Class.forName("android.app.ActivityThread");
            Field f = at.getDeclaredField("mProviderMap");
            f.setAccessible(true);
            Object map = f.get(sActivityThread);
            if (map == null) { warnings.add("settings_provider_no_map"); return; }

            Class<?> icp = Class.forName("android.content.IContentProvider");
            final Object[] binder = new Object[1];
            Class<?> ibinder = Class.forName("android.os.IBinder");
            binder[0] = java.lang.reflect.Proxy.newProxyInstance(
                    ibinder.getClassLoader(), new Class<?>[]{ibinder}, (p, m, a) -> {
                        String n = m.getName();
                        if (n.equals("isBinderAlive") || n.equals("pingBinder")) return Boolean.TRUE;
                        if (n.equals("toString")) return "ACOS local settings provider";
                        if (n.equals("hashCode")) return System.identityHashCode(p);
                        if (n.equals("equals")) return p == (a == null ? null : a[0]);
                        if (n.equals("queryLocalInterface")) return null;
                        return defaultOf(m.getReturnType());
                    });
            Object provider = java.lang.reflect.Proxy.newProxyInstance(
                    icp.getClassLoader(), new Class<?>[]{icp}, (p, m, a) -> {
                        String n = m.getName();
                        if (n.equals("asBinder")) return binder[0];
                        if (n.equals("toString")) return "ACOS local settings IContentProvider";
                        if (n.equals("hashCode")) return System.identityHashCode(p);
                        if (n.equals("equals")) return p == (a == null ? null : a[0]);
                        if (n.equals("call")) return new android.os.Bundle();
                        if (n.equals("query")) {
                            String[] projection = a != null && a.length > 2 ? (String[]) a[2] : null;
                            return new android.database.MatrixCursor(
                                    projection != null ? projection : new String[]{"value"});
                        }
                        return defaultOf(m.getReturnType());
                    });

            Class<?> pk = Class.forName("android.app.ActivityThread$ProviderKey");
            Constructor<?> pkc = pk.getDeclaredConstructor(String.class, int.class);
            pkc.setAccessible(true);
            Object key = pkc.newInstance("settings", 0);

            Class<?> pcr = Class.forName("android.app.ActivityThread$ProviderClientRecord");
            Constructor<?> pcrc = pcr.getDeclaredConstructor(
                    at, String[].class, icp,
                    Class.forName("android.content.ContentProvider"),
                    Class.forName("android.app.ContentProviderHolder"));
            pcrc.setAccessible(true);
            Object rec = pcrc.newInstance(sActivityThread, new String[]{"settings"}, provider, null, null);
            ((java.util.Map) map).put(key, rec);
            note("local settings ContentProvider registered in ActivityThread.mProviderMap");
        } catch (Throwable t) {
            warnings.add("settings_provider_failed=" + unwrap(t));
            note("local settings provider -> " + unwrap(t));
        }
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
            // The real system ContextImpl: getDefaultActivityIcon() (reached by
            // PhoneWindow.installDecor for the action bar's up icon) calls
            // mContext.getDrawable(), so a null context NPEs.
            sPackageManager = (android.content.pm.PackageManager) c.newInstance(sSystemContext, pm);
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

    /** Window-attribute evidence (flags pick which insets the decor consumes). */
    static void probeWindow(int stage, View decor) {
        try {
            android.view.WindowManager.LayoutParams lp = sWindow.getAttributes();
            StringBuilder sb = new StringBuilder("window[" + stage + "] flags=0x"
                    + Integer.toHexString(lp.flags) + " systemUi=0x"
                    + Integer.toHexString(lp.systemUiVisibility));
            try {
                Field fd = sWindow.getClass().getDeclaredField("mDecorFitsSystemWindows");
                fd.setAccessible(true);
                sb.append(" decorFits=").append(fd.get(sWindow));
            } catch (NoSuchFieldException ignored) {
            }
            for (Class<?> c = decor.getClass(); c != null; c = c.getSuperclass()) {
                try {
                    Field fd = c.getDeclaredField("mForceWindowDrawsBarBackgrounds");
                    fd.setAccessible(true);
                    sb.append(" forceBarBg=").append(fd.get(decor));
                    break;
                } catch (NoSuchFieldException ignored) {
                }
            }
            note(sb.toString());
        } catch (Throwable t) {
            note("window probe -> " + unwrap(t));
        }
    }

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

    // ------------------------------------------------------------- the window

    /**
     * Build the window the way the framework does, without ViewRootImpl/WMS:
     * a real {@code com.android.internal.policy.PhoneWindow} over the activity
     * theme, {@code setContentView(layout)} (which runs the stock
     * {@code installDecor}/{@code generateLayout}: DecorView, ActionBarOverlayLayout,
     * ActionBarContainer, Toolbar, id/content) and the resulting DecorView.
     * Everything inflated, measured and laid out below it is stock code; no
     * node position is set by this guest.
     */
    static View buildWindow(Context ctx) throws Throwable {
        Class<?> pw = Class.forName("com.android.internal.policy.PhoneWindow");
        Constructor<?> c = pw.getDeclaredConstructor(Context.class);
        c.setAccessible(true);
        android.view.Window w = (android.view.Window) c.newInstance(ctx);
        sWindow = w;
        // Window.setWindowManager() casts to WindowManagerImpl; the local
        // window manager is exactly what ActivityThread hands an Activity.
        try {
            Object wm = ctx.getSystemService(Context.WINDOW_SERVICE);
            w.setWindowManager((android.view.WindowManager) wm, null, sPkg, false);
            note("setWindowManager(" + (wm == null ? "null" : wm.getClass().getName()) + ")");
        } catch (Throwable t) {
            warnings.add("window_manager=" + unwrap(t));
            note("setWindowManager -> " + unwrap(t));
        }
        if (sTitle != null && !sTitle.isEmpty()) w.setTitle(sTitle);
        long t0 = System.nanoTime();
        w.setContentView(sLayoutId);
        View decor = w.getDecorView();
        probeWindow(1, decor);
        note("PhoneWindow.setContentView(0x" + Integer.toHexString(sLayoutId) + ") title=\""
             + sTitle + "\" -> " + decor.getClass().getName() + " in "
             + (System.nanoTime() - t0) / 1000 + " us");
        return decor;
    }

    /**
     * The ViewRootImpl half we do not have: dispatch the window's insets (the
     * oracle's status/navigation bar frames), measure at the window frame and
     * lay the decor out at (0,0). Insets come from argv, so the guest
     * hardcodes no system-bar height.
     */
    static void layoutWindow(View decor) {
        try {
            android.graphics.Insets sys = android.graphics.Insets.of(0, STATUS_INSET, 0, NAV_INSET);
            android.view.WindowInsets insets = new android.view.WindowInsets.Builder()
                    .setSystemWindowInsets(sys)
                    .setStableInsets(sys)
                    .build();
            decor.dispatchApplyWindowInsets(insets);
            note("dispatchApplyWindowInsets(system=" + sys + ") -> decor padding l/t/r/b="
                 + decor.getPaddingLeft() + "/" + decor.getPaddingTop() + "/"
                 + decor.getPaddingRight() + "/" + decor.getPaddingBottom());
        } catch (Throwable t) {
            warnings.add("insets_dispatch_failed=" + unwrap(t));
            note("dispatchApplyWindowInsets -> " + unwrap(t));
        }
        // DecorView.onLayout() (and a couple of window callbacks) dereference
        // getViewRootImpl(); there is no ViewRootImpl here. Give the decor the
        // minimal AttachInfo a window would have -- after the insets are
        // dispatched, so updateColorViews() takes the no-ViewRootImpl branch.
        installFakeViewRootImpl(decor);
        probeWindow(2, decor);
        long t0 = System.nanoTime();
        decor.measure(View.MeasureSpec.makeMeasureSpec(W, View.MeasureSpec.EXACTLY),
                      View.MeasureSpec.makeMeasureSpec(PANEL_H, View.MeasureSpec.EXACTLY));
        decor.layout(0, 0, W, PANEL_H);
        note("window measure+layout " + W + "x" + PANEL_H + " insets="
             + STATUS_INSET + "/" + NAV_INSET + " in " + (System.nanoTime() - t0) / 1000 + " us");
    }

    /**
     * A zeroed {@code ViewRootImpl} (Unsafe.allocateInstance, no constructor:
     * the real one needs IWindowSession/WMS) behind a View$AttachInfo, so
     * `View.getViewRootImpl()` answers instead of NPEing. Only
     * `requestInvalidateRootRenderNode()` (a boolean write) is reached on it.
     */
    static void installFakeViewRootImpl(View decor) {
        try {
            Class<?> unsafeCls = Class.forName("sun.misc.Unsafe");
            Field uf = unsafeCls.getDeclaredField("theUnsafe");
            uf.setAccessible(true);
            Object unsafe = uf.get(null);
            Method alloc = unsafeCls.getMethod("allocateInstance", Class.class);

            ClassLoader cl = decor.getClass().getClassLoader();
            Class<?> vri = Class.forName("android.view.ViewRootImpl", false, cl);
            Class<?> ai = Class.forName("android.view.View$AttachInfo", false, cl);
            Object root = alloc.invoke(unsafe, vri);
            Object attach = alloc.invoke(unsafe, ai);
            Field fVri = ai.getDeclaredField("mViewRootImpl");
            fVri.setAccessible(true);
            fVri.set(attach, root);
            // A non-null AttachInfo makes View.sizeChange() rebuild the outline
            // from AttachInfo.mTmpOutline into the view's RenderNode; supply both
            // (a display-list RenderNode needs no GPU/surface).
            Field fOutline = ai.getDeclaredField("mTmpOutline");
            fOutline.setAccessible(true);
            fOutline.set(attach, new android.graphics.Outline());
            Field fAttach = View.class.getDeclaredField("mAttachInfo");
            fAttach.setAccessible(true);
            fAttach.set(decor, attach);
            Field fNode = View.class.getDeclaredField("mRenderNode");
            fNode.setAccessible(true);
            if (fNode.get(decor) == null) {
                fNode.set(decor, new android.graphics.RenderNode("decor"));
            }
            note("installed minimal View$AttachInfo/ViewRootImpl (no WMS)");
        } catch (Throwable t) {
            warnings.add("fake_view_root=" + unwrap(t));
            note("fake ViewRootImpl -> " + unwrap(t));
        }
    }

    // ------------------------------------------------------------------ draw

    static int[] draw(View root) throws Throwable {
        int bg = windowBackgroundColor();
        Bitmap bm = Bitmap.createBitmap(W, PANEL_H, Bitmap.Config.ARGB_8888);
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
        int[] px = new int[W * PANEL_H];
        bm.getPixels(px, 0, W, 0, 0, W, PANEL_H);
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
                     View root, String tree, int[] drawUs) {
        StringBuilder sb = new StringBuilder();
        sb.append("{\"outcome\":\"").append(outcome).append("\"");
        if (code != null) sb.append(",\"code\":\"").append(esc(code)).append("\"");
        if (message != null) sb.append(",\"message\":\"").append(esc(message)).append("\"");
        sb.append(",\"width\":").append(W).append(",\"contentHeight\":").append(CONTENT_H)
          .append(",\"panelHeight\":").append(PANEL_H)
          .append(",\"statusInset\":").append(STATUS_INSET)
          .append(",\"navInset\":").append(NAV_INSET)
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
