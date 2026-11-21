package dev.acos.art;

import android.app.Activity;
import android.app.Application;
import android.app.Instrumentation;
import android.content.ComponentName;
import android.content.Context;
import android.content.Intent;
import android.content.pm.ActivityInfo;
import android.content.res.Configuration;
import android.content.res.Resources;
import android.graphics.Bitmap;
import android.graphics.Canvas;
import android.os.Binder;
import android.os.Bundle;
import android.os.Handler;
import android.os.Looper;
import android.os.SystemClock;
import android.view.Display;
import android.view.KeyEvent;
import android.view.MotionEvent;
import android.view.View;
import android.view.Window;

import java.io.BufferedReader;
import java.io.File;
import java.io.FileOutputStream;
import java.io.InputStreamReader;
import java.io.OutputStreamWriter;
import java.io.PrintStream;
import java.io.Writer;
import java.lang.reflect.Constructor;
import java.lang.reflect.Field;
import java.lang.reflect.Method;
import java.security.MessageDigest;
import java.util.ArrayList;
import java.util.List;

/**
 * `acos-art session` guest: **one long-lived ART process** that runs a real
 * {@link Activity} from the APK and serves a line-JSON command protocol on
 * stdin/stdout (`screenshot`, `tree`, `tap`, `key`, `reset`, `close`).
 *
 * The point of the session (vs {@code render-app}) is that **the app's own code
 * runs**: the guest instantiates the manifest's launcher Activity through its
 * real constructor, calls the framework's {@code attach(...)} (the same 19-arg
 * method {@code ActivityThread.handleLaunchActivity} calls) and then
 * {@code performCreate}/{@code performStart}/{@code performResume}. So
 * {@code MainActivity.onCreate} executes for real — {@code setContentView},
 * {@code findViewById} and above all the app's own
 * {@code setOnClickListener(...)} closures are installed by the app, not by us.
 *
 * Touches are equally real: {@code {"cmd":"tap"}} builds a
 * {@link MotionEvent} DOWN/UP pair (times from {@link SystemClock#uptimeMillis})
 * and hands it to {@code DecorView.dispatchTouchEvent} → {@code Activity
 * .dispatchTouchEvent} → {@code PhoneWindow.superDispatchTouchEvent} → the real
 * {@code ViewGroup} hit-testing, so the listener that fires is the one the app
 * registered. No {@code performClick}, no synthetic state change anywhere.
 *
 * All of the window construction is {@link AppMain}'s: the same rungs (graphics
 * natives, {@code ActivityThread.systemMain()}, the image's Resources +
 * static RRO overlays, the font map, the local settings provider, the stub
 * IWMS/IME) — this class only replaces "inflate the layout bare" with "launch
 * the Activity" and adds the command loop.
 *
 * argv: apk pkg outDir width contentHeight densityDpi
 *       idmap,idmap,... panelHeight statusInset navInset themeHex title
 *       targetSdk activityClass sessionId
 */
public final class SessionMain {

    static String sActivityClass;
    static String sSessionId = "";
    static View sDecor;
    static Activity sActivity;
    static Object sActivityThread;
    static Handler sHandler;
    static BufferedReader sIn;
    static PrintStream sCmdOut;
    static Writer sVerdictOut;

    static int sSeq;
    static String sLastPng = "";
    static String sLastMd5 = "";
    static String sLastTree = "{}";
    static int sLastWidth, sLastHeight;
    static int sFramesDrawn;
    static long sDrawUs;

    static final List<String> sNotes = new ArrayList<>();
    static final List<String> sWarnings = new ArrayList<>();

    /** `null` = the process is still serving commands. */
    static String sPending;
    static long sCmdStartedNs;
    static String sCmdName = "";

    public static void main(String[] args) {
        // System.currentTimeMillis(): main() runs *before* the SystemClock
        // natives are registered (stepRuntime), so the wall clock is wall-only.
        long t0 = System.currentTimeMillis();
        sCmdOut = new PrintStream(new FileOutputStream(java.io.FileDescriptor.out), true);
        sVerdictOut = new OutputStreamWriter(new FileOutputStream(java.io.FileDescriptor.out));
        sIn = new BufferedReader(new InputStreamReader(System.in));
        // The startup phase is a command too: a wedge here must dump its stack.
        sCmdName = "startup";
        sCmdStartMs = System.currentTimeMillis();
        startStuckWatchdog();
        try {
            parseArgs(args);
            new File(AppMain.sOut).mkdirs();
            setup();
            launch();
            publishFrame();
            emitReady();
            sHandler = new Handler(Looper.getMainLooper());
            sCmdName = "";
            sCmdStartMs = 0;
            sHandler.post(SessionMain::readNext);
            Looper.loop();
        } catch (Throwable t) {
            Throwable u = AppMain.unwrap(t);
            AppMain.dumpChain(u);
            cmdError("startup", "STARTUP_FAILED", String.valueOf(u));
            verdict("crash", "STARTUP_FAILED", String.valueOf(u), t0);
            System.exit(3);
        }
    }

    // ------------------------------------------------------------------ args

    /** Same argv contract as {@link AppMain}, plus activity + session id. */
    static void parseArgs(String[] args) throws Throwable {
        AppMain.sApk = args[0];
        AppMain.sPkg = args[1];
        AppMain.sOut = args[2];
        if (args.length > 3) AppMain.W = Integer.parseInt(args[3]);
        if (args.length > 4) AppMain.CONTENT_H = Integer.parseInt(args[4]);
        if (args.length > 5) AppMain.DENSITY = Integer.parseInt(args[5]);
        if (args.length > 6 && !args[6].isEmpty()) AppMain.sIdmaps = args[6].split(",");
        if (args.length > 7) AppMain.PANEL_H = Integer.parseInt(args[7]);
        if (args.length > 8) AppMain.STATUS_INSET = Integer.parseInt(args[8]);
        if (args.length > 9) AppMain.NAV_INSET = Integer.parseInt(args[9]);
        if (args.length > 10) AppMain.sThemeId = AppMain.parseTheme(args[10]);
        if (args.length > 11) AppMain.sTitle = args[11];
        if (args.length > 12) AppMain.sTargetSdk = Integer.parseInt(args[12]);
        if (args.length > 13) sActivityClass = args[13];
        if (args.length > 14) sSessionId = args[14];
        if (sActivityClass == null || sActivityClass.isEmpty()) {
            throw new IllegalArgumentException("no activity class on argv");
        }
    }

    /** The same rungs {@code AppMain.run()} walks, minus the bare inflate. */
    static void setup() throws Throwable {
        long t0 = System.currentTimeMillis();
        AppMain.stepGfx();
        AppMain.stepRuntime();
        stepRuntimeExtras();
        AppMain.ensureActivity();
        sActivityThread = AppMain.sActivityThread;
        AppMain.stepResources();
        AppMain.installStubWindowManager();
        AppMain.seedTypeface();
        sDisplay = makeDisplay();
        sCtx = new DisplayContext(AppMain.newContext(), sDisplay);
        installLocalPackageManagerService();
        installLocalActivityTaskManager();
        note("setup in " + (System.currentTimeMillis() - t0) + " ms");
    }

    /**
     * Natives the **activity** path needs and the bare-inflate path does not
     * reach. `Activity.performCreate` logs through
     * `EventLogTags.writeWmOnCreateCalled` -> `EventLog.writeEvent(int,
     * Object[])`, i.e. a JNI method of `libandroid_runtime`; unregistered it is a
     * hard `UnsatisfiedLinkError` in the middle of the framework's launch.
     * Registering the *real* implementation (never a Java stub) keeps the log
     * write a real log write.
     */
    static void stepRuntimeExtras() {
        String[] extra = {
                "register_android_util_EventLog",
                "register_android_os_Debug",
                "register_android_os_StatFs",
                "register_android_os_HidlSupport",
                "register_android_util_SeempLog",
                "register_android_view_InputChannel",
                "register_android_view_KeyEvent",
                "register_android_view_MotionEvent",
                // WV4 (wall 8): `android.net.LocalSocketImpl`'s `AF_UNIX`
                // natives (connect/read/write/peer-credentials). Any guest
                // transport that runs over a Unix socket — the WebView
                // provider's session socket (`WIRE.md` §0) is the first one —
                // needs the image's own registration; unregistered it is
                // `UnsatisfiedLinkError: android.net.LocalSocketImpl
                // .connectLocal`. Same root cause as LaunchMain's list.
                "register_android_net_LocalSocketImpl",
                // MODA11YNATIVE: `AccessibilityService.takeScreenshot` hands the
                // app a `HardwareBuffer`; `nReadHardwareBufferFromParcel` and the
                // `nGet*` getters are the image's own implementations, and
                // libui's `GraphicBuffer` unflatten goes through the ACOS
                // gralloc shim (which owns the memfd), so no substitute native
                // is needed.
                "register_android_hardware_HardwareBuffer",
                // CAMERA2/3: `CameraMetadataNative` and `SurfaceUtils` JNI backed by
                // the image's own `libcamera_client.so` + `libcamera_metadata.so` +
                // `libgui.so` (same as LaunchMain).
                "register_android_hardware_camera2_CameraMetadata",
                "register_android_hardware_camera2_utils_SurfaceUtils",
        };
        int ok = 0, miss = 0;
        for (String n : extra) {
            int rc;
            try {
                rc = RenderNat.register(n);
            } catch (Throwable t) {
                rc = -1;
                note("register(" + n + ") threw " + AppMain.unwrap(t));
            }
            if (rc == 1) ok++; else { miss++; note("register(" + n + ") -> " + rc); }
        }
        if (miss > 0) sWarnings.add("activity_natives_missing=" + miss);
        note("activity-native register ok=" + ok + " missing=" + miss);
    }

    static Display sDisplay;
    static Context sCtx;

    /**
     * The one display this environment has, built from the **argv** panel
     * metrics (1080x2400 @ 420 dpi for the oracle). `ContextImpl
     * .getDisplayNoVerify()` goes through `ResourcesManager.getAdjustedDisplay`
     * → `DisplayManagerGlobal` → `ServiceManager.getService("display")`, i.e.
     * the binder peer we do not have, so it returns null and
     * `DecorContext.<init>` NPEs on `display.getDisplayId()`. Handing the window
     * a `Display` for the panel the caller asked for is the same class of input
     * as the status/navigation insets: environment geometry, not app data.
     */
    static Display makeDisplay() {
        try {
            Class<?> unsafeCls = Class.forName("sun.misc.Unsafe");
            Field uf = unsafeCls.getDeclaredField("theUnsafe");
            uf.setAccessible(true);
            Object unsafe = uf.get(null);
            android.view.Display d = (android.view.Display) unsafeCls
                    .getMethod("allocateInstance", Class.class)
                    .invoke(unsafe, android.view.Display.class);
            // DisplayInfo is @hide: build it reflectively with the panel metrics.
            Class<?> diCls = Class.forName("android.view.DisplayInfo");
            Object info = diCls.getDeclaredConstructor().newInstance();
            putIf(info, "name", "ACOS display");
            putIf(info, "logicalWidth", AppMain.W);
            putIf(info, "logicalHeight", AppMain.PANEL_H);
            putIf(info, "appWidth", AppMain.W);
            putIf(info, "appHeight", AppMain.PANEL_H);
            putIf(info, "densityDpi", AppMain.DENSITY);
            putIf(info, "logicalDensityDpi", AppMain.DENSITY);
            put(d, "mDisplayId", android.view.Display.DEFAULT_DISPLAY);
            putIf(d, "mDisplayInfo", info);
            putIf(d, "mResources", AppMain.sRes);
            note("display: id=0 " + AppMain.W + "x" + AppMain.PANEL_H + "@" + AppMain.DENSITY
                    + " (argv panel, no DisplayManagerService)");
            return d;
        } catch (Throwable t) {
            sWarnings.add("display_stub=" + AppMain.unwrap(t));
            note("display stub -> " + AppMain.unwrap(t));
            return null;
        }
    }

    /** Field write for the @hide fields above; absent names are not fatal. */
    static void putIf(Object target, String name, Object value) {
        try {
            put(target, name, value);
        } catch (Throwable ignored) {
        }
    }

    /** Field write that also works for final fields (mDisplayId). */
    static void put(Object target, String name, Object value) throws Throwable {
        for (Class<?> c = target.getClass(); c != null; c = c.getSuperclass()) {
            try {
                Field f = c.getDeclaredField(name);
                f.setAccessible(true);
                f.set(target, value);
                return;
            } catch (NoSuchFieldException ignored) {
            }
        }
        throw new NoSuchFieldException(name);
    }

    /**
     * The second binder service the *activity* path needs and `render-app`
     * never touched: `ActivityTaskManager` (a framework singleton over
     * `ServiceManager.getService("activity_task")`). API-34's
     * `ActivityClient` singleton calls
     * `ActivityTaskManager.getService().getActivityClientController()` while the
     * window styles are read, so a null service is an NPE in the middle of
     * `PhoneWindow.generateLayout`.
     *
     * Same substitution as the settings provider and the IWMS/IME stubs: no
     * servicemanager here, so a local `IActivityTaskManager` +
     * `IActivityClientController` pair answers the calls with defaults. The
     * framework's own singleton cache is reset so it picks the local instance up;
     * every call that used to be a binder transaction is now a no-op, and none
     * of them changes app-visible state that this environment has (there is no
     * task stack, no WMS, no ANR bookkeeping).
     */
    static void installLocalActivityTaskManager() {
        try {
            Class<?> ibinder = Class.forName("android.os.IBinder");
            Class<?> iatm = Class.forName("android.app.IActivityTaskManager");
            Class<?> iacc = Class.forName("android.app.IActivityClientController");
            // A local proxy must be *both* the binder and the interface:
            // Stub.asInterface(b) checks b.queryLocalInterface(DESCRIPTOR) first.
            final Object[] self = new Object[2];
            Object controller = java.lang.reflect.Proxy.newProxyInstance(
                    iacc.getClassLoader(), new Class<?>[]{ibinder, iacc},
                    (p, m, a) -> {
                        String n = m.getName();
                        if (n.equals("queryLocalInterface")) return self[1];
                        if (n.equals("getInterfaceDescriptor")) return iacc.getName();
                        if (n.equals("isBinderAlive") || n.equals("pingBinder")) return Boolean.TRUE;
                        if (n.equals("toString")) return "ACOS stub IActivityClientController";
                        if (n.equals("hashCode")) return System.identityHashCode(p);
                        if (n.equals("equals")) return p == (a == null ? null : a[0]);
                        return AppMain.defaultOf(m.getReturnType());
                    });
            self[1] = controller;
            Object atm = java.lang.reflect.Proxy.newProxyInstance(
                    iatm.getClassLoader(), new Class<?>[]{ibinder, iatm},
                    (p, m, a) -> {
                        String n = m.getName();
                        if (n.equals("queryLocalInterface")) return self[0];
                        if (n.equals("getInterfaceDescriptor")) return iatm.getName();
                        if (n.equals("isBinderAlive") || n.equals("pingBinder")) return Boolean.TRUE;
                        if (n.equals("toString")) return "ACOS stub IActivityTaskManager";
                        if (n.equals("hashCode")) return System.identityHashCode(p);
                        if (n.equals("equals")) return p == (a == null ? null : a[0]);
                        if (n.startsWith("getActivityClientController")) {
                            note("stub IActivityTaskManager." + n + " -> local controller");
                            return controller;
                        }
                        return AppMain.defaultOf(m.getReturnType());
                    });
            self[0] = atm;
            // ServiceManager cache: what IActivityTaskManager.Stub.asInterface()
            // is handed inside the framework's own Singleton.
            Class<?> sm = Class.forName("android.os.ServiceManager");
            Field sc = sm.getDeclaredField("sCache");
            sc.setAccessible(true);
            ((java.util.Map) sc.get(null)).put("activity_task", atm);
            // ... and drop the already-created (null) singleton so it is rebuilt.
            Class<?> atmsCls = Class.forName("android.app.ActivityTaskManager");
            for (Field f : atmsCls.getDeclaredFields()) {
                if (!java.lang.reflect.Modifier.isStatic(f.getModifiers())) continue;
                if (!f.getType().getName().equals("android.util.Singleton")) continue;
                f.setAccessible(true);
                Object singleton = f.get(null);
                if (singleton == null) continue;
                Field mi = Class.forName("android.util.Singleton").getDeclaredField("mInstance");
                mi.setAccessible(true);
                Object old = mi.get(singleton);
                mi.set(singleton, null);
                note("reset " + atmsCls.getSimpleName() + "." + f.getName() + " (was " + old + ")");
            }
            note("ActivityTaskManager -> local IActivityTaskManager (+IActivityClientController)");
        } catch (Throwable t) {
            sWarnings.add("activity_task_stub=" + AppMain.unwrap(t));
            note("local ActivityTaskManager -> " + AppMain.unwrap(t));
        }
    }

    /**
     * The activity's base context: {@link AppMain.MiniContext} (the app's
     * Resources, its theme, the app's ApplicationInfo) plus the environment's
     * display, so framework code that asks the context for a `Display` gets the
     * panel instead of a null.
     */
    static final class DisplayContext extends android.view.ContextThemeWrapper {
        private final Context base;
        private final Display display;
        DisplayContext(Context base, Display display) {
            super(base, android.R.style.Theme_DeviceDefault);
            this.base = base;
            this.display = display;
        }
        // Context.getDisplayNoVerify() is @hide (absent from android.jar), so no
        // @Override: the signature is what the runtime dispatches on.
        public Display getDisplayNoVerify() {
            return display != null ? display : super.getDisplay();
        }
        @Override public Display getDisplay() {
            return display != null ? display : super.getDisplay();
        }
        @Override public Context getApplicationContext() { return base; }
        @Override public Resources getResources() { return base.getResources(); }
        @Override public Resources.Theme getTheme() {
            return base.getTheme() != null ? base.getTheme() : super.getTheme();
        }
    }

    /**
     * `ApplicationPackageManager.hasSystemFeature()` does **not** use the
     * instance's `mPM`: it queries the static
     * `ActivityThread.getPackageManager()` cache (API-34 bytecode), which in a
     * bare `dalvikvm64` is null because it comes from
     * `ServiceManager.getService("package")`. Same substitution as the guest's
     * `IWindowManager`/`IInputMethodManager` stubs: there is no servicemanager
     * here, so a local `IPackageManager` answers "not supported" — the honest
     * answer for a minimal environment (no feature is silently claimed).
     */
    static void installLocalPackageManagerService() {
        try {
            Class<?> at = Class.forName("android.app.ActivityThread");
            Class<?> ipm = Class.forName("android.content.pm.IPackageManager");
            final int[] calls = {0};
            Object proxy = java.lang.reflect.Proxy.newProxyInstance(
                    ipm.getClassLoader(), new Class<?>[]{ipm}, (p, m, a) -> {
                        if (m.getName().equals("hasSystemFeature") && calls[0]++ < 8) {
                            note("stub IPackageManager." + m.getName() + "(" + (a == null ? "" : a[0]) + ") -> false");
                        }
                        return AppMain.defaultOf(m.getReturnType());
                    });
            Field f = at.getDeclaredField("sPackageManager");
            f.setAccessible(true);
            Object old = f.get(null);
            f.set(null, proxy);
            note("ActivityThread.sPackageManager = local IPackageManager (was " + old + ")");
        } catch (Throwable t) {
            sWarnings.add("ipackage_manager_stub=" + AppMain.unwrap(t));
            note("local IPackageManager -> " + AppMain.unwrap(t));
        }
    }

    // ------------------------------------------------- the real Activity launch

    /**
     * Instantiate the APK's launcher Activity from **its own** {@code
     * classes.dex} and drive the framework's launch sequence:
     * {@code attach(...)} (19-arg, the one {@code ActivityThread} calls) →
     * {@code performCreate} → {@code performStart} → {@code performResume}.
     *
     * Everything after {@code attach} is the app's code path: the window is
     * created by {@code Activity.attach} ({@code new PhoneWindow(this, ...)}),
     * the content view by the app's own {@code setContentView}.
     */
    static void launch() throws Throwable {
        long t0 = System.currentTimeMillis();
        ClassLoader loader = new dalvik.system.PathClassLoader(AppMain.sApk,
                ClassLoader.getSystemClassLoader());
        Class<?> cls = Class.forName(sActivityClass, true, loader);
        Activity a = (Activity) cls.getDeclaredConstructor().newInstance();
        note("loaded " + sActivityClass + " from the APK's classes.dex (" + loader + ")");

        Class<?> at = Class.forName("android.app.ActivityThread");
        Method instrM = at.getDeclaredMethod("getInstrumentation");
        instrM.setAccessible(true);
        Instrumentation instr = (Instrumentation) instrM.invoke(sActivityThread);
        Method appM = at.getDeclaredMethod("getApplication");
        appM.setAccessible(true);
        Application app = (Application) appM.invoke(sActivityThread);
        if (instr == null || app == null) {
            throw new IllegalStateException("ActivityThread instrumentation/application missing: "
                    + instr + "/" + app);
        }

        ActivityInfo info = new ActivityInfo();
        info.name = sActivityClass;
        info.packageName = AppMain.sPkg;
        info.applicationInfo = sCtx.getApplicationInfo();
        info.targetActivity = sActivityClass;

        Intent intent = new Intent(Intent.ACTION_MAIN);
        intent.setComponent(new ComponentName(AppMain.sPkg, sActivityClass));

        Class<?> nci = Class.forName("android.app.Activity$NonConfigurationInstances");
        Class<?> vic = Class.forName("com.android.internal.app.IVoiceInteractor");
        Class<?> accb = Class.forName("android.view.ViewRootImpl$ActivityConfigCallback");
        Method attach = Activity.class.getDeclaredMethod("attach",
                Context.class, at, Instrumentation.class, android.os.IBinder.class,
                int.class, Application.class, Intent.class, ActivityInfo.class,
                CharSequence.class, Activity.class, String.class, nci,
                Configuration.class, String.class, vic, Window.class, accb,
                android.os.IBinder.class, android.os.IBinder.class);
        attach.setAccessible(true);
        attach.invoke(a, sCtx, sActivityThread, instr, new Binder(), 0, app, intent, info,
                AppMain.sTitle, null, "acos-session", null, AppMain.sRes.getConfiguration(),
                null, null, null, null, null, null);
        note("Activity.attach() -> " + a.getWindow().getClass().getName());

        Method create = Activity.class.getDeclaredMethod("performCreate", Bundle.class);
        create.setAccessible(true);
        long tc = System.nanoTime();
        create.invoke(a, (Bundle) null);
        long createUs = (System.nanoTime() - tc) / 1000;

        // onStart/onResume are the device's post-launch state; the counter's UI
        // does not change there, but the lifecycle must be the real one.
        String resumed = "none";
        try {
            Method start = Activity.class.getDeclaredMethod("performStart", String.class);
            start.setAccessible(true);
            start.invoke(a, (String) null);
            Method resume = Activity.class.getDeclaredMethod("performResume", boolean.class, String.class);
            resume.setAccessible(true);
            resume.invoke(a, true, null);
            resumed = "onCreate->onStart->onResume";
        } catch (Throwable t) {
            sWarnings.add("lifecycle_after_create=" + AppMain.unwrap(t));
            note("performStart/performResume -> " + AppMain.unwrap(t));
        }

        sActivity = a;
        AppMain.sWindow = a.getWindow();
        sDecor = a.getWindow().getDecorView();
        note("activity onCreate in " + createUs + " us, lifecycle " + resumed
                + ", decor=" + sDecor.getClass().getName());
        AppMain.probeWindow(1, sDecor);

        // The insets the oracle's system bars give the window, then the
        // ViewRootImpl half we substitute (AttachInfo + zeroed ViewRootImpl).
        dispatchInsets();
        installAttachInfo(sDecor);
        // A ripple on a *software* canvas SIGSEGVs inside libhwui (E4H §3.4), so
        // every RippleDrawable is replaced by its content layer once, up front —
        // before any touch, so pressed-state changes never reach the ripple.
        int n = AppMain.downgradeRipples(sDecor);
        if (n > 0) note("downgraded " + n + " RippleDrawable -> content layer");
        // A pressed Button runs a StateListAnimator (elevation/translation). Its
        // first use constructs Choreographer -> DisplayEventReceiver -> a
        // SurfaceFlinger binder transaction, and this environment has no binder
        // transport: the shim's fake driver retries the ioctl forever and the
        // tap never returns (measured: DOWN at a Button wedged for >4 s). A
        // published frame is a static snapshot with no vsync source, so the
        // static-frame-honest substitution is to drop the animator (it changes
        // no pixel of the unpressed/pressed *content*, only z-elevation).
        int anim = downgradeStateListAnimators(sDecor);
        if (anim > 0) note("downgraded " + anim + " StateListAnimator -> none (no vsync source)");
        note("launch in " + (System.currentTimeMillis() - t0) + " ms");
    }

    static void dispatchInsets() {
        try {
            android.graphics.Insets sys = android.graphics.Insets.of(0, AppMain.STATUS_INSET,
                    0, AppMain.NAV_INSET);
            android.view.WindowInsets insets = new android.view.WindowInsets.Builder()
                    .setSystemWindowInsets(sys)
                    .setStableInsets(sys)
                    .build();
            sDecor.dispatchApplyWindowInsets(insets);
            note("dispatchApplyWindowInsets(system=" + sys + ")");
        } catch (Throwable t) {
            sWarnings.add("insets_dispatch_failed=" + AppMain.unwrap(t));
        }
    }

    /**
     * {@link AppMain#installFakeViewRootImpl} plus what a *touch* needs: the
     * whole tree actually attached.
     *
     * `installFakeViewRootImpl` only sets `AttachInfo` on the decor, and that is
     * enough to draw. It is **not** enough for input: on {@code ACTION_UP} the
     * framework posts the click (`View.post(mPerformClick)`), and a view whose
     * `mAttachInfo` is null queues it in its `HandlerActionQueue` instead — to be
     * run by `dispatchAttachedToWindow`, which never came. The app's
     * `OnClickListener` then never fires (measured: the tap was handled, the
     * tree still said "0"). So dispatch the real
     * `View.dispatchAttachedToWindow(AttachInfo, int)` from the decor down, with
     * `AttachInfo.mHandler` installed first: every view gets `mAttachInfo`, runs
     * `onAttachedToWindow`, and flushes exactly the run queue a real attach
     * would have flushed. Also clears {@code mViewRequestingLayout}, which a real
     * traversal clears at its start.
     */
    static void installAttachInfo(View decor) throws Throwable {
        AppMain.installFakeViewRootImpl(decor);
        Class<?> ai = Class.forName("android.view.View$AttachInfo");
        Field fAttach = View.class.getDeclaredField("mAttachInfo");
        fAttach.setAccessible(true);
        Object attach = fAttach.get(decor);
        if (attach == null) throw new IllegalStateException("no View$AttachInfo on the decor");
        Field h = ai.getDeclaredField("mHandler");
        h.setAccessible(true);
        h.set(attach, new Handler(Looper.getMainLooper()));
        Field req = ai.getDeclaredField("mViewRequestingLayout");
        req.setAccessible(true);
        req.set(attach, null);
        sAttachInfo = attach;
        sHandler = new Handler(Looper.getMainLooper());

        // @hide, package-private in android.view: reflect, do not stub.
        //
        // The attach starts at the decor's *children*, not at the DecorView:
        // DecorView.onAttachedToWindow expects window-manager objects this
        // environment does not build (WindowOnBackInvokedDispatcher, window
        // callbacks on a real ViewRootImpl) and none of it is needed to
        // hit-test or to post a click. Every descendant still gets mAttachInfo
        // and flushes exactly the run queue a real attach flushes.
        Method dispatchAttach = View.class.getDeclaredMethod(
                "dispatchAttachedToWindow", ai, int.class);
        dispatchAttach.setAccessible(true);
        int attached = 0;
        if (decor instanceof android.view.ViewGroup) {
            android.view.ViewGroup g = (android.view.ViewGroup) decor;
            for (int i = 0; i < g.getChildCount(); i++) {
                View child = g.getChildAt(i);
                fAttach.set(child, null);
                dispatchAttach.invoke(child, attach, 0);
                attached++;
            }
        }
        note("dispatchAttachedToWindow on " + attached
                + " decor child(ren) (mAttachInfo + run queues)");
    }

    static Object sAttachInfo;

    /** Drop every StateListAnimator in the tree (see the note in {@link #launch}). */
    static int downgradeStateListAnimators(View root) {
        int n = 0;
        List<View> stack = new ArrayList<>();
        stack.add(root);
        while (!stack.isEmpty()) {
            View v = stack.remove(stack.size() - 1);
            try {
                if (v.getStateListAnimator() != null) {
                    v.setStateListAnimator(null);
                    n++;
                }
            } catch (Throwable ignored) {
            }
            if (v instanceof android.view.ViewGroup) {
                android.view.ViewGroup g = (android.view.ViewGroup) v;
                for (int i = 0; i < g.getChildCount(); i++) stack.add(g.getChildAt(i));
            }
        }
        return n;
    }

    // ------------------------------------------------------------- traversal

    /** What {@code ViewRootImpl.performTraversals} does with the flags it finds. */
    static void traversal() {
        try {
            if (sAttachInfo != null) {
                Field req = sAttachInfo.getClass().getDeclaredField("mViewRequestingLayout");
                req.setAccessible(true);
                req.set(sAttachInfo, null);
            }
        } catch (Throwable ignored) {
        }
        sDecor.measure(View.MeasureSpec.makeMeasureSpec(AppMain.W, View.MeasureSpec.EXACTLY),
                View.MeasureSpec.makeMeasureSpec(AppMain.PANEL_H, View.MeasureSpec.EXACTLY));
        sDecor.layout(0, 0, AppMain.W, AppMain.PANEL_H);
    }

    /**
     * Re-run the traversal, draw the window and publish a **new** frame:
     * {@code frame_<seq>.png} + md5. {@code frame_seq} only advances once the
     * pixels are complete (PNG written and hashed), so a reply that carries a
     * seq also carries that frame's bytes.
     */
    static void publishFrame() throws Throwable {
        long t0 = System.nanoTime();
        trace("publish: traversal");
        traversal();
        trace("publish: tree");
        sLastTree = AppMain.dumpTree(sDecor);
        int bg = AppMain.windowBackgroundColor();
        Bitmap bm = Bitmap.createBitmap(AppMain.W, AppMain.PANEL_H, Bitmap.Config.ARGB_8888);
        bm.eraseColor(bg);
        Canvas canvas = new Canvas(bm);
        long d0 = System.nanoTime();
        trace("publish: draw");
        sDecor.draw(canvas);
        trace("publish: draw done");
        sDrawUs = (System.nanoTime() - d0) / 1000;
        java.io.ByteArrayOutputStream bos = new java.io.ByteArrayOutputStream(1 << 20);
        if (!bm.compress(Bitmap.CompressFormat.PNG, 100, bos)) {
            throw new IllegalStateException("Bitmap.compress(PNG) refused");
        }
        byte[] png = bos.toByteArray();
        bm.recycle();
        String md5 = md5(png);
        // A frame exists once its pixels are complete: the sequence number and
        // the md5 are assigned together, after the PNG bytes are final.
        int seq = sSeq + 1;
        File f = new File(AppMain.sOut, "frame_" + seq + ".png");
        try (FileOutputStream fos = new FileOutputStream(f)) {
            fos.write(png);
        }
        sSeq = seq;
        sLastPng = f.getPath();
        sLastMd5 = md5;
        sLastWidth = AppMain.W;
        sLastHeight = AppMain.PANEL_H;
        sFramesDrawn++;
        note("frame_seq=" + sSeq + " md5=" + sLastMd5 + " draw=" + sDrawUs + " us publish="
                + (System.nanoTime() - t0) / 1000 + " us");
    }

    static String md5(byte[] data) throws Throwable {
        MessageDigest d = MessageDigest.getInstance("MD5");
        byte[] h = d.digest(data);
        StringBuilder sb = new StringBuilder(h.length * 2);
        for (byte b : h) sb.append(Character.forDigit((b >> 4) & 0xf, 16))
                .append(Character.forDigit(b & 0xf, 16));
        return sb.toString();
    }

    // --------------------------------------------------------------- protocol

    /**
     * One command per line from stdin. Dispatch happens in this message; the
     * frame is published from a *follow-up* message so that every runnable the
     * dispatch posted on the main Looper (View.PerformClick, UnsetPressedState)
     * has run first — exactly the order a real ViewRootImpl gives.
     */
    static void readNext() {
        String line;
        try {
            line = sIn.readLine();
        } catch (Throwable t) {
            line = null;
        }
        if (line == null) {
            cmdError("close", "EOF", "stdin closed");
            shutdown("eof");
        }
        sCmdStartedNs = System.nanoTime();
        sCmdStartMs = System.currentTimeMillis();
        sDumped = false;
        sPending = line;
        try {
            dispatchCommand(line);
        } catch (Throwable t) {
            Throwable u = AppMain.unwrap(t);
            AppMain.dumpChain(u);
            cmdError(sCmdName, "GUEST_ERROR", String.valueOf(u));
            sHandler.post(SessionMain::readNext);
            return;
        }
        sHandler.post(SessionMain::finishCommand);
    }

    /** Phase A: mutate state (dispatch input, recreate the Activity, …). */
    static void dispatchCommand(String line) throws Throwable {
        String cmd = jsonString(line, "cmd");
        sCmdName = cmd == null ? "" : cmd;
        if (cmd == null) throw new IllegalArgumentException("no cmd in: " + line);
        switch (cmd) {
            case "screenshot":
            case "tree":
                return;
            case "tap": {
                float x = (float) jsonNumber(line, "x", 0);
                float y = (float) jsonNumber(line, "y", 0);
                sTapX = x;
                sTapY = y;
                return;
            }
            case "key": {
                sKeyCode = (int) jsonNumber(line, "code", 0);
                return;
            }
            case "reset":
                relaunch();
                return;
            case "close":
                return;
            default:
                throw new IllegalArgumentException("unknown cmd " + cmd);
        }
    }

    static float sTapX, sTapY;
    static int sKeyCode;
    static boolean sTapHandled, sKeyHandled;
    static long sTapDispatchUs, sKeyDispatchUs;

    /**
     * Phase B: dispatch the input. Nothing is measured or drawn here: the
     * framework posts the click itself (`View.post(mPerformClick)` from
     * {@code ACTION_UP}) and that runnable must run *before* the frame is
     * published, exactly as it would before a real traversal. So this phase
     * only hands the event over and schedules phase C.
     */
    static void finishCommand() {
        try {
            long t0 = System.nanoTime();
            switch (sCmdName) {
                case "tap":
                    sTapHandled = tap(sTapX, sTapY);
                    sTapDispatchUs = (System.nanoTime() - t0) / 1000;
                    break;
                case "key":
                    sKeyHandled = key(sKeyCode);
                    sKeyDispatchUs = (System.nanoTime() - t0) / 1000;
                    break;
                default:
                    break;
            }
        } catch (Throwable t) {
            Throwable u = AppMain.unwrap(t);
            AppMain.dumpChain(u);
            cmdError(sCmdName, "GUEST_ERROR", String.valueOf(u));
            sCmdStartMs = 0;
            sHandler.post(SessionMain::readNext);
            return;
        }
        sHandler.post(SessionMain::completeCommand);
    }

    /**
     * Phase C: the state the app's own runnables produced is now final, so
     * re-traverse, draw, publish the new frame and answer.
     */
    static void completeCommand() {
        // `close` publishes nothing: no new pixels, so no new frame_seq.
        if (!sCmdName.equals("close")) {
            try {
                publishFrame();
            } catch (Throwable t) {
                Throwable u = AppMain.unwrap(t);
                cmdError(sCmdName, "PUBLISH_FAILED", String.valueOf(u));
                sCmdStartMs = 0;
                sHandler.post(SessionMain::readNext);
                return;
            }
        }
        try {
            switch (sCmdName) {
                case "screenshot":
                    replyFrame("screenshot");
                    break;
                case "tree":
                    replyTree();
                    break;
                case "tap":
                    replyTap();
                    break;
                case "key":
                    replyKey();
                    break;
                case "reset":
                    replyReset();
                    break;
                case "close":
                    replyClose();
                    break;
                default:
                    cmdError(sCmdName, "BAD_CMD", "unknown cmd");
                    break;
            }
        } catch (Throwable t) {
            cmdError(sCmdName, "GUEST_ERROR", String.valueOf(AppMain.unwrap(t)));
        }
        if (sCmdName.equals("close")) {
            shutdown("close");
        }
        sCmdStartMs = 0;
        sHandler.post(SessionMain::readNext);
    }

    /** A real DOWN/UP pair through the window's real dispatch chain. */
    static boolean tap(float x, float y) {
        long down = SystemClock.uptimeMillis();
        MotionEvent d = MotionEvent.obtain(down, down, MotionEvent.ACTION_DOWN, x, y, 0);
        trace("tap: DOWN " + x + "," + y);
        boolean downHandled = sDecor.dispatchTouchEvent(d);
        d.recycle();
        trace("tap: DOWN handled=" + downHandled);
        // A real tap is ~50 ms on a device; the framework's click detection
        // (tap timeout, long-press window) only needs eventTime >= downTime.
        long up = Math.max(SystemClock.uptimeMillis(), down + 1);
        MotionEvent u = MotionEvent.obtain(down, up, MotionEvent.ACTION_UP, x, y, 0);
        trace("tap: UP");
        boolean upHandled = sDecor.dispatchTouchEvent(u);
        u.recycle();
        trace("tap: UP handled=" + upHandled);
        note("tap(" + x + "," + y + ") down=" + downHandled + " up=" + upHandled
                + " holdMs=" + (up - down));
        return downHandled && upHandled;
    }

    static boolean key(int code) {
        long t = SystemClock.uptimeMillis();
        KeyEvent d = new KeyEvent(t, t, KeyEvent.ACTION_DOWN, code, 0);
        boolean down = sDecor.dispatchKeyEvent(d);
        KeyEvent u = new KeyEvent(t, t + 1, KeyEvent.ACTION_UP, code, 0);
        boolean up = sDecor.dispatchKeyEvent(u);
        note("key(" + code + ") down=" + down + " up=" + up);
        return down || up;
    }

    /** `reset`: destroy this Activity the way the framework does and launch a
     * fresh one in the same process (fresh {@code onCreate}, count back to 0). */
    static void relaunch() throws Throwable {
        long t0 = System.currentTimeMillis();
        try {
            Method destroy = Activity.class.getDeclaredMethod("performDestroy");
            destroy.setAccessible(true);
            destroy.invoke(sActivity);
            note("performDestroy on the previous activity");
        } catch (Throwable t) {
            sWarnings.add("performDestroy=" + AppMain.unwrap(t));
        }
        sActivity = null;
        sDecor = null;
        sFramesDrawn = 0;
        launch();
        note("relaunch in " + (System.currentTimeMillis() - t0) + " ms");
    }

    // ------------------------------------------------------------------ replies

    static void emitReady() {
        StringBuilder sb = new StringBuilder();
        sb.append("{\"ok\":true,\"cmd\":\"ready\"");
        sb.append(",\"session\":\"").append(AppMain.esc(sSessionId)).append('"');
        sb.append(",\"apk\":\"").append(AppMain.esc(AppMain.sApk)).append('"');
        sb.append(",\"package\":\"").append(AppMain.esc(AppMain.sPkg)).append('"');
        sb.append(",\"activity\":\"").append(AppMain.esc(sActivityClass)).append('"');
        sb.append(",\"pid\":").append(android.os.Process.myPid());
        sb.append(",\"width\":").append(AppMain.W).append(",\"height\":").append(AppMain.PANEL_H);
        sb.append(",\"densityDpi\":").append(AppMain.DENSITY);
        sb.append(",\"frame_seq\":").append(sSeq);
        sb.append(",\"lifecycle\":\"onCreate->onStart->onResume\"");
        sb.append(",\"png\":\"").append(AppMain.esc(sLastPng)).append('"');
        sb.append(",\"md5\":\"").append(sLastMd5).append('"');
        sb.append(",\"draw_us\":").append(sDrawUs);
        sb.append(",\"tree\":").append(sLastTree);
        sb.append('}');
        cmdLine(sb.toString());
    }

    static void replyFrame(String cmd) {
        StringBuilder sb = frameJson(cmd);
        sb.append('}');
        cmdLine(sb.toString());
    }

    static void replyTree() {
        StringBuilder sb = frameJson("tree");
        sb.append(",\"tree\":").append(sLastTree).append('}');
        cmdLine(sb.toString());
    }

    static void replyTap() {
        StringBuilder sb = frameJson("tap");
        sb.append(",\"tap\":{\"handled\":").append(sTapHandled)
          .append(",\"x\":").append(sTapX).append(",\"y\":").append(sTapY)
          .append(",\"dispatch_us\":").append(sTapDispatchUs).append('}');
        sb.append(",\"tree\":").append(sLastTree).append('}');
        cmdLine(sb.toString());
    }

    static void replyKey() {
        StringBuilder sb = frameJson("key");
        sb.append(",\"key\":{\"code\":").append(sKeyCode)
          .append(",\"handled\":").append(sKeyHandled)
          .append(",\"dispatch_us\":").append(sKeyDispatchUs).append('}');
        sb.append(",\"tree\":").append(sLastTree).append('}');
        cmdLine(sb.toString());
    }

    static void replyReset() {
        StringBuilder sb = frameJson("reset");
        sb.append(",\"tree\":").append(sLastTree).append('}');
        cmdLine(sb.toString());
    }

    static void replyClose() {
        StringBuilder sb = frameJson("close");
        sb.append('}');
        cmdLine(sb.toString());
    }

    /** The fields every frame-carrying reply has. */
    static StringBuilder frameJson(String cmd) {
        StringBuilder sb = new StringBuilder();
        sb.append("{\"ok\":true,\"cmd\":\"").append(cmd).append('"');
        sb.append(",\"frame_seq\":").append(sSeq);
        sb.append(",\"png\":\"").append(AppMain.esc(sLastPng)).append('"');
        sb.append(",\"md5\":\"").append(sLastMd5).append('"');
        sb.append(",\"width\":").append(sLastWidth).append(",\"height\":").append(sLastHeight);
        sb.append(",\"draw_us\":").append(sDrawUs);
        sb.append(",\"frames_drawn\":").append(sFramesDrawn);
        sb.append(",\"guest_ms\":").append((System.nanoTime() - sCmdStartedNs) / 1000000);
        return sb;
    }

    static void cmdError(String cmd, String code, String message) {
        String sb = "{\"ok\":false,\"cmd\":\"" + cmd + "\",\"code\":\"" + code
                + "\",\"error\":\"" + AppMain.esc(message) + "\"}";
        cmdLine(sb);
    }

    static void cmdLine(String json) {
        sCmdOut.println("ACOSCMD " + json);
        sCmdOut.flush();
    }

    static void shutdown(String why) {
        verdict("ok", null, why, System.currentTimeMillis());
        System.exit(0);
    }

    static void verdict(String outcome, String code, String message, long t0) {
        try {
            StringBuilder sb = new StringBuilder();
            sb.append("{\"outcome\":\"").append(outcome).append('"');
            if (code != null) sb.append(",\"code\":\"").append(code).append('"');
            if (message != null) sb.append(",\"message\":\"").append(AppMain.esc(message)).append('"');
            sb.append(",\"width\":").append(AppMain.W).append(",\"contentHeight\":")
              .append(AppMain.CONTENT_H).append(",\"panelHeight\":").append(AppMain.PANEL_H)
              .append(",\"statusInset\":").append(AppMain.STATUS_INSET)
              .append(",\"navInset\":").append(AppMain.NAV_INSET)
              .append(",\"densityDpi\":").append(AppMain.DENSITY)
              .append(",\"nodes\":").append(AppMain.countNodes(sLastTree))
              .append(",\"draw_cold_us\":").append(sDrawUs)
              .append(",\"frames_drawn\":").append(sFramesDrawn)
              .append(",\"frame_seq\":").append(sSeq)
              .append(",\"peak_rss_kb\":").append(AppMain.peakRssKb())
              .append(",\"wall_ms\":").append(System.currentTimeMillis() - t0);
            sb.append(",\"notes\":[");
            List<String> all = new ArrayList<>(AppMain.notes);
            all.addAll(sNotes);
            for (int i = 0; i < all.size(); i++) {
                if (i > 0) sb.append(',');
                sb.append('"').append(AppMain.esc(all.get(i))).append('"');
            }
            sb.append("],\"warnings\":[");
            List<String> warn = new ArrayList<>(AppMain.warnings);
            warn.addAll(sWarnings);
            for (int i = 0; i < warn.size(); i++) {
                if (i > 0) sb.append(',');
                sb.append('"').append(AppMain.esc(warn.get(i))).append('"');
            }
            sb.append("]}");
            sCmdOut.println("ACOSAPP " + sb);
            sCmdOut.flush();
        } catch (Throwable ignored) {
        }
    }

    static void note(String s) {
        sNotes.add(s);
        trace(s);
    }

    /**
     * A wedged command must be diagnosable from outside. There is no ptrace in
     * this environment, so the process dumps its own main-thread stack (once per
     * command) when a command has been running for {@link #STUCK_MS}. The dump
     * goes to stderr, which the host streams to `<session>/guest-stderr.txt`.
     */
    static final long STUCK_MS = 4000;
    static Thread sMainThread;
    static volatile long sCmdStartMs;
    static volatile boolean sDumped;

    static void startStuckWatchdog() {
        sMainThread = Thread.currentThread();
        Thread t = new Thread(() -> {
            while (true) {
                try {
                    Thread.sleep(500);
                } catch (InterruptedException e) {
                    return;
                }
                long start = sCmdStartMs;
                if (start == 0 || sDumped) continue;
                if (System.currentTimeMillis() - start < STUCK_MS) continue;
                sDumped = true;
                try {
                    System.err.println("ACOSSTUCK cmd=" + sCmdName
                            + " running for " + (System.currentTimeMillis() - start) + " ms");
                    for (StackTraceElement e : sMainThread.getStackTrace()) {
                        System.err.println("  at " + e);
                    }
                    System.err.flush();
                } catch (Throwable ignored) {
                }
            }
        }, "acos-stuck-watchdog");
        t.setDaemon(true);
        t.start();
    }

    /**
     * Live trace to the guest's stderr (bounded), so a session that wedges
     * mid-command can still be diagnosed: the notes that would only be flushed
     * in the final verdict are already on disk by then.
     */
    static int sTrace;

    static void trace(String s) {
        if (sTrace++ > 4000) return;
        try {
            System.err.println("ACOSTRACE " + s);
            System.err.flush();
        } catch (Throwable ignored) {
        }
    }

    // --------------------------------------------------------- tiny JSON input

    /** `"cmd":"tap"` → tap. Deliberately minimal: the protocol is one line. */
    static String jsonString(String line, String key) {
        String pat = "\"" + key + "\"";
        int i = line.indexOf(pat);
        if (i < 0) return null;
        int c = line.indexOf(':', i + pat.length());
        if (c < 0) return null;
        int q1 = line.indexOf('"', c + 1);
        if (q1 < 0) return null;
        int q2 = line.indexOf('"', q1 + 1);
        if (q2 < 0) return null;
        return line.substring(q1 + 1, q2);
    }

    static double jsonNumber(String line, String key, double def) {
        String pat = "\"" + key + "\"";
        int i = line.indexOf(pat);
        if (i < 0) return def;
        int c = line.indexOf(':', i + pat.length());
        if (c < 0) return def;
        int end = c + 1;
        while (end < line.length()
                && (Character.isWhitespace(line.charAt(end)) || line.charAt(end) == '-'
                    || line.charAt(end) == '+' || line.charAt(end) == '.'
                    || Character.isDigit(line.charAt(end)))) {
            end++;
        }
        try {
            return Double.parseDouble(line.substring(c + 1, end).trim());
        } catch (Throwable t) {
            return def;
        }
    }
}
