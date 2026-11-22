package dev.acos.art;

import android.app.Application;
import android.app.Instrumentation;
import android.content.ComponentName;
import android.content.pm.ApplicationInfo;
import android.content.res.Configuration;
import android.os.Bundle;
import android.os.IBinder;
import android.os.Looper;
import android.os.Parcel;
import android.view.View;
import android.view.accessibility.AccessibilityNodeInfo;
import android.view.accessibility.AccessibilityNodeProvider;
import android.view.ViewGroup;
import android.view.ViewTreeObserver;
import android.view.accessibility.AccessibilityNodeInfo;
import android.widget.TextView;

import java.io.File;
import java.io.FileOutputStream;
import java.lang.reflect.Constructor;
import java.lang.reflect.Field;
import java.lang.reflect.InvocationTargetException;
import java.lang.reflect.Method;
import java.util.ArrayList;

/**
 * `acos-art launch` guest: the real framework startup path.
 *
 * <ol>
 *   <li>{@code Looper.prepareMainLooper()} and a real
 *       {@code new ActivityThread()} (private constructor, reflection);</li>
 *   <li>marshal an {@code IApplicationThread.bindApplication} request with the
 *       framework's own {@code Parcel.write*} calls and hand the bytes to the
 *       {@code acos.launch} peer node (P4a: parcel-passed reverse delivery);</li>
 *   <li>{@code ActivityThread.attach(false, startSeq)} — the real
 *       {@code IActivityManager.attachApplication(mAppThread, startSeq)}
 *       transaction to the Rust {@code ServiceHost};</li>
 *   <li>{@code Looper.loop()}: the peer answers attach and delivers
 *       {@code bindApplication} to the real {@code ApplicationThread} binder;
 *       {@code ActivityThread.handleBindApplication} builds
 *       {@code LoadedApk}, the {@code Application} from the APK, and (because
 *       {@code instrumentationName == null}) the framework itself constructs
 *       {@code new Instrumentation()} and calls
 *       {@code callApplicationOnCreate(app)} — <b>no class from the guest</b>;
 *   <li>for {@code --until activity-resumed} the watcher then marshals one
 *       {@code ClientTransaction} ({@code LaunchActivityItem} +
 *       {@code ResumeActivityItem}) with the framework's own
 *       {@code writeToParcel}, pushes it to the peer, and the peer delivers it
 *       as {@code IApplicationThread.scheduleTransaction} — the real AMS step.
 *       {@code ActivityThread} then runs {@code Activity.onCreate/onResume};
 *       the watcher publishes {@code activity-resumed} only after the resume
 *       message and the first window traversal have both completed.</li>
 * </ol>
 *
 * argv: apk guestPackage outDir [until] [launcherActivity]
 */
public final class LaunchMain {
    static String sApk;
    static String sPkg;
    static String sOut;
    static String sUntil = "application-created";
    static String sActivity;
    /** `<application android:name>`, fully qualified; empty = platform default. */
    static String sAppClass = "";
    /** `<application android:theme>` resource id; 0 = platform default. */
    static int sAppTheme;
    static volatile boolean sApplicationCreated;
    /** `<application android:label>` resource id (0 = none). */
    static int sAppLabelRes;
    /** `<application android:label>` literal string (empty = none). */
    static String sAppLabelText = "";
    /** The launched `<activity android:label>` resource id (0 = none). */
    static int sActivityLabelRes;
    /** The launched `<activity android:label>` literal string (empty = none). */
    static String sActivityLabelText = "";
    /** Static-RRO overlay idmaps (`/data/overlay/<name>.idmap`, `,`-separated). */
    static String sOverlayIdmaps = "";
    /**
     * W27: the alias target (`ActivityInfo.targetActivity`). Empty for a plain
     * activity; when set, `ActivityThread.performLaunchActivity` replaces the
     * intent's alias component with this class before instantiating it.
     */
    static String sTargetActivity = "";
    /** W27: effective `Activity.mResumed` wait (`ACOSLAUNCH_RESUME_MS`). */
    static long sResumeMs = 12000L;
    /**
     * O3: hard upper bound on the post-resume readiness wait
     * (`ACOSLAUNCH_RESUME_READY_MS`): `Activity.mResumed` observed → the first
     * `ViewRootImpl.performTraversals` draw completed. Exceeding it is a hard
     * failure (structured marker + {@code POST_RESUME_NOT_READY}), never a
     * silent "ready".
     */
    static long sResumeReadyMs = 2000L;
    /**
     * W31: `ApplicationInfo.flags` as `PackageParser` derived them from the
     * APK's manifest, plus the package manager's post-install
     * `FLAG_INSTALLED`. `-1` = the host supplied none (older callers): keep the
     * historical `FLAG_HAS_CODE | FLAG_INSTALLED` value so such a caller is not
     * silently changed. The host clears `FLAG_HARDWARE_ACCELERATED` before
     * passing it (P4b: the surface-less ACOS window only draws through the
     * software `ViewRootImpl` path).
     */
    static int sAppFlags = -1;
    /** W31: where `sAppFlags` came from (`manifest`, `env`, or `default`). */
    static String sAppFlagsSource = "default";
    /** P12: the agent's animation choice for this session (`ACOS_ANIMATIONS`). */
    static boolean sAnimationsRequested = true;
    static String sAnimationsRequestSource = "default";
    /**
     * Frame-probe progress: `-1` = probe disabled, otherwise the number of
     * completed `doFrame` callbacks. The watcher waits for this before sending
     * the activity transaction so the probe evidence is not cut short by the
     * window-path exception (it is a timing guard for a diagnostic run only).
     */
    static volatile int sProbeFrames = -1;
    static final int PROBE_TARGET_FRAMES = 8;
    static final long[] sProbeDeltas = new long[PROBE_TARGET_FRAMES];
    static volatile long sProbeFirst;
    static volatile long sProbeLast;

    public static void main(String[] args) {
        long t0 = System.currentTimeMillis();
        if (args.length < 3) {
            System.out.println("ACOSLAUNCH {\"outcome\":\"crash\",\"code\":\"BAD_ARGS\"}");
            System.exit(2);
        }
        sApk = args[0];
        sPkg = args[1];
        sOut = args[2];
        if (args.length > 3 && args[3] != null && args[3].length() > 0) sUntil = args[3];
        if (args.length > 4 && args[4] != null && args[4].length() > 0) sActivity = args[4];
        if (args.length > 5 && args[5] != null) sAppClass = args[5];
        if (args.length > 6 && args[6] != null && args[6].length() > 0) {
            try {
                sAppTheme = Integer.parseInt(args[6].trim());
            } catch (NumberFormatException ignored) {
                sAppTheme = 0;
            }
        }
        // W24: static-RRO idmaps plus the manifest label fields
        // `PackageParser` writes into ApplicationInfo/ActivityInfo.
        if (args.length > 7 && args[7] != null) sOverlayIdmaps = args[7];
        if (args.length > 8 && args[8] != null && args[8].length() > 0) {
            sAppLabelRes = parseIntOrZero(args[8]);
        }
        if (args.length > 9 && args[9] != null) sAppLabelText = args[9];
        if (args.length > 10 && args[10] != null && args[10].length() > 0) {
            sActivityLabelRes = parseIntOrZero(args[10]);
        }
        if (args.length > 11 && args[11] != null) sActivityLabelText = args[11];
        if (args.length > 12 && args[12] != null) sTargetActivity = args[12];
        // W31: manifest-derived `ApplicationInfo.flags` (decimal; `-1`/absent =
        // none supplied).
        if (args.length > 13 && args[13] != null && args[13].trim().length() > 0) {
            try {
                sAppFlags = (int) Long.parseLong(args[13].trim());
            } catch (NumberFormatException ignored) {
                sAppFlags = -1;
            }
        }
        if (args.length > 14 && args[14] != null && args[14].trim().length() > 0) {
            sAppFlagsSource = args[14].trim();
        }
        // W27: the resume budget is configurable. The host picks a documented
        // default from the APK shape and forwards it; an explicit value from
        // the environment is clammped to a sane deadline.
        try {
            String resume = System.getenv("ACOSLAUNCH_RESUME_MS");
            if (resume != null && resume.trim().length() > 0) {
                long ms = Long.parseLong(resume.trim());
                if (ms >= 1000L && ms <= 600000L) sResumeMs = ms;
            }
            String ready = System.getenv("ACOSLAUNCH_RESUME_READY_MS");
            if (ready != null && ready.trim().length() > 0) {
                long ms = Long.parseLong(ready.trim());
                // A readiness bound of zero or "infinite" is not a bound.
                if (ms >= 100L && ms <= 60000L) sResumeReadyMs = ms;
            }
        } catch (Throwable ignored) {
        }
        // P12: per-session animation scales. The host forwards `ACOS_ANIMATIONS`
        // (default `on`: real framework animation behaviour).
        try {
            String anim = System.getenv("ACOS_ANIMATIONS");
            if (anim != null && anim.trim().length() > 0) {
                String v = anim.trim();
                if ("off".equalsIgnoreCase(v)) {
                    sAnimationsRequested = false;
                    sAnimationsRequestSource = "env:off";
                } else if ("on".equalsIgnoreCase(v)) {
                    sAnimationsRequested = true;
                    sAnimationsRequestSource = "env:on";
                } else {
                    System.out.println("ACOSLAUNCH WARN ACOS_ANIMATIONS=" + v
                            + " is not on|off; keeping animations on");
                    sAnimationsRequestSource = "env:invalid";
                }
            }
        } catch (Throwable ignored) {
        }
        System.out.println("ACOSLAUNCH MARK resume-budget ms=" + sResumeMs
                + " ready_ms=" + sResumeReadyMs);
        System.out.flush();
        // A device's zygote installs the AndroidKeyStore provider once, before
        // any app process exists (ZygoteInit.warmUpJcaProviders). ACOS starts
        // dalvikvm64 directly, so the software provider is installed here —
        // before app code, which is where the platform has it. Session-private
        // storage: keys live under this run's /data/app-data only.
        //
        // OPT-IN: the provider is a compatibility shim (exportable software
        // keys, no KeyInfo/attestation claims), not device security. It is off
        // unless `ACOS_ART_KEYSTORE=on` so a normal run keeps the honest
        // "AndroidKeyStore not found" failure until the K5b fix list lands.
        try {
            String ks = System.getenv("ACOS_ART_KEYSTORE");
            String mode = ks == null ? "" : ks.trim();
            if ("on".equalsIgnoreCase(mode) || "1".equals(mode) || "software".equalsIgnoreCase(mode)) {
                SoftwareKeyStore.install("/data/app-data/" + sPkg);
            }
        } catch (Throwable t) {
            System.out.println("ACOSLAUNCH WARN keystore install: " + t);
        }
        try {
            run(t0);
        } catch (Throwable t) {
            Throwable u = unwrap(t);
            u.printStackTrace(System.out);
            System.out.println("ACOSLAUNCH {\"outcome\":\"crash\",\"code\":\"GUEST_ERROR\",\"message\":\""
                    + esc(String.valueOf(u)) + "\"" + crashDetail(u) + "}");
            System.out.flush();
            System.exit(1);
        }
    }

    static int parseIntOrZero(String s) {
        try {
            return Integer.parseInt(s.trim());
        } catch (NumberFormatException ignored) {
            return 0;
        }
    }

    /**
     * W24: apply the image's **immutable** framework RROs the way
     * `AssetManager.createSystemAssetsInZygoteLocked` does on a device.
     *
     * <p>The zygote path loads `framework-res.apk` plus one
     * `ApkAssets.loadOverlayFromPath(idmap, PROPERTY_SYSTEM)` per immutable
     * overlay idmap reported by `OverlayConfig` (framework static overlays are
     * always on; nothing here is a switch). The ACOS sysroot has no
     * `/product/overlay`, so `OverlayConfig` finds nothing and the system
     * AssetManager would hold the un-overlaid `framework-res.apk` — whose
     * `config_buttonTextAllCaps` is `true`, against the product RRO's `false`
     * (W18 #2). {@code acos-art} generates the idmaps with the image's own
     * `/system/bin/idmap2` and binds them at `/data/overlay`; this method adds
     * them to the *system* asset list, which {@code AssetManager.Builder} then
     * copies into every app {@code Resources} (it always starts with
     * `getSystem().getApkAssets()`).
     *
     * <p>Everything used here is stock framework code — `ApkAssets`,
     * `AssetManager.getApkAssets`, `setApkAssets` — no ACOS resource code and no
     * hardcoded attribute. An unusable idmap is reported and skipped, never
     * faked.
     */
    static void applySystemOverlays() {
        if (sOverlayIdmaps.isEmpty()) {
            System.out.println("ACOSLAUNCH MARK overlays none (no static RRO idmap supplied)");
            return;
        }
        try {
            Class<?> amc = android.content.res.AssetManager.class;
            Class<?> aac = Class.forName("android.content.res.ApkAssets");
            int systemFlag = 0x1;
            try {
                java.lang.reflect.Field f = aac.getDeclaredField("PROPERTY_SYSTEM");
                f.setAccessible(true);
                systemFlag = f.getInt(null);
            } catch (Throwable ignored) {
            }
            Object system = amc.getMethod("getSystem").invoke(null);
            Object[] base = (Object[]) amc.getMethod("getApkAssets").invoke(system);
            java.lang.reflect.Method loadOverlay = null;
            for (java.lang.reflect.Method m : aac.getDeclaredMethods()) {
                if (!m.getName().equals("loadOverlayFromPath")) continue;
                Class<?>[] p = m.getParameterTypes();
                if (p.length == 2 && p[0] == String.class && p[1] == int.class) {
                    loadOverlay = m;
                    break;
                }
            }
            if (loadOverlay == null) throw new NoSuchMethodException("ApkAssets.loadOverlayFromPath(String,int)");
            loadOverlay.setAccessible(true);
            java.util.List<Object> all = new java.util.ArrayList<>();
            for (Object a : base) all.add(a);
            int loaded = 0;
            for (String idmap : sOverlayIdmaps.split(",")) {
                if (idmap.isEmpty()) continue;
                try {
                    all.add(loadOverlay.invoke(null, idmap, systemFlag));
                    loaded++;
                } catch (Throwable t) {
                    System.out.println("ACOSLAUNCH MARK overlay-skipped " + idmap + " " + unwrap(t));
                }
            }
            Object[] array = all.toArray(new Object[0]);
            Object typed = java.lang.reflect.Array.newInstance(aac, array.length);
            for (int i = 0; i < array.length; i++) java.lang.reflect.Array.set(typed, i, array[i]);
            amc.getMethod("setApkAssets", typed.getClass(), boolean.class)
                    .invoke(system, typed, Boolean.TRUE);
            System.out.println("ACOSLAUNCH MARK overlays applied=" + loaded + " base=" + base.length
                    + " total=" + array.length + " idmaps=" + sOverlayIdmaps);
        } catch (Throwable t) {
            System.out.println("ACOSLAUNCH MARK overlays failed " + unwrap(t));
        }
    }

    /**
     * GATENE2E: the guest paints every window on a software {@code Canvas}
     * ({@code ViewRootImpl.drawSoftware}); it has no window GPU renderer
     * (docs/LATER.md "GPU-drawn windows"). The host already clears
     * {@code FLAG_HARDWARE_ACCELERATED} on the app and activities, but a window
     * the app adds itself can ask for acceleration explicitly (Gaten's lookup
     * overlay sets {@code FLAG_HARDWARE_ACCELERATED}); {@code ViewRootImpl.
     * enableHardwareAcceleration} then calls {@code ThreadedRenderer.create},
     * whose {@code ProcessInitializer} aborts the RenderThread
     * ({@code DeviceInfo::updateDisplayInfo}: no SurfaceFlinger physical
     * display). {@code ThreadedRenderer.sRendererEnabled} is the framework's own
     * switch for "this process must not do accelerated rendering" (low-end
     * persistent processes): with it false the window falls back to the software
     * path and {@code Canvas.isHardwareAccelerated()} is false, exactly what an
     * app sees on a device without a window GPU. The GL provider stays available
     * for bitmap readbacks (hwui's {@code RenderProxy}), which do not use it.
     */
    static void disableWindowRenderer() {
        try {
            Class<?> tr = Class.forName("android.view.ThreadedRenderer");
            tr.getField("sRendererEnabled").setBoolean(null, false);
            System.out.println("ACOSLAUNCH MARK window-renderer software (ThreadedRenderer.sRendererEnabled=false)");
        } catch (Throwable t) {
            System.out.println("ACOSLAUNCH MARK window-renderer-disable failed " + unwrap(t));
        }
    }

    static void run(long t0) throws Throwable {
        registerNatives();
        disableWindowRenderer();
        new File(sOut).mkdirs();
        Looper.prepareMainLooper();
        // A real Android process answers `IApplicationThread` transactions on a
        // binder looper thread (RuntimeInit starts the pool); the framework's
        // ApplicationThread then posts BIND_APPLICATION to the main looper.
        // A bare dalvikvm has no pool: the driver exports the same
        // IPCThreadState::joinThreadPool entry the P2f rung uses.
        Nat.load();
        // app_main normally constructs AndroidRuntime before the framework
        // runs; without it Process.setArgV0 (handleBindApplication) faults.
        int runtime = initAndroidRuntime(sPkg);
        if (runtime < 0) throw new IllegalStateException("initAndroidRuntime -> " + runtime);
        System.out.println("ACOSLAUNCH MARK android-runtime ready rc=" + runtime);
        // AOSP's process bootstrap: `ActivityThread.main` runs
        // `initializeMainlineModules()` once per app process, before
        // `Looper.prepareMainLooper()`/`attach()` and therefore long before
        // BIND_APPLICATION. It installs the mainline `ServiceManager` holders
        // the framework's service managers read in their constructors
        // (`TelephonyServiceManager`, `StatsServiceManager`, the media platform +
        // media `MediaServiceManager`, `BluetoothServiceManager`,
        // `NfcServiceManager`, `DeviceConfigServiceManager`).
        //
        // On a device every app process gets it from that one entry point. ACOS
        // builds the `ActivityThread` itself and never runs `main()`, so without
        // this call the holders stay null for *every* guest process and the
        // framework's own managers die in their constructors
        // (MODA11Y7: `MediaSessionManager.<init>` -> `MediaServiceManager` null,
        // Gaten's accessibility service `onCreate`). The list is the framework's
        // own, not a per-module guess: a module declares a
        // `process_initializer` only for a manager this call does not install
        // (see `ModuleManifest::process_initializers`), so nothing registers
        // twice.
        initializeMainlineModules();
        // ADR-0029 item 1: the extra framework initializers a module needs this
        // *process* to run and that AOSP's own `initializeMainlineModules()`
        // above does not install. The launcher forwards the attached modules'
        // `ModuleManifest::process_initializers` as `class#method#manager`
        // entries joined by ';'; the guest runs them here, exactly once, before
        // the app boots. Telephony and Bluetooth are *not* here: AOSP's call
        // already installs both, and their setters throw on a second call, so
        // declaring them would be a double registration. A session whose modules
        // declare none forwards nothing. Adding a module never touches this
        // code: it is one reflective loop over the manifest's data.
        String initializers = System.getenv("ACOSLAUNCH_INITIALIZERS");
        if (initializers != null && !initializers.isEmpty()) {
            for (String entry : initializers.split(";")) {
                if (entry.isEmpty()) continue;
                String[] fields = entry.split("#");
                if (fields.length != 3) {
                    System.out.println("ACOSLAUNCH WARN process-initializer malformed " + entry);
                    continue;
                }
                try {
                    // Both classes are hidden framework API (not in the SDK jar
                    // this guest source compiles against), so they are reached
                    // by reflection, the same way AppMain reaches `ActivityThread`
                    // and the platform font internals.
                    Class<?> initializerClass = Class.forName(fields[0]);
                    Class<?> managerClass = Class.forName(fields[2]);
                    Object manager = managerClass.getConstructor().newInstance();
                    java.lang.reflect.Method set =
                            initializerClass.getDeclaredMethod(fields[1], managerClass);
                    set.setAccessible(true);
                    set.invoke(null, manager);
                    System.out.println("ACOSLAUNCH MARK process-initializer installed "
                            + fields[0] + "#" + fields[1]);
                } catch (Throwable t) {
                    System.out.println("ACOSLAUNCH WARN process-initializer " + entry + ": "
                            + unwrap(t));
                }
            }
        }
        // R3: the runtime is up, so libsigchain exists with ART's own fault
        // handlers already in the chain. Installing ours now appends it *after*
        // them: it runs only on a fault ART declined, prints the fault address,
        // signal code, thread and ART's own `Backtrace`, and never claims the
        // signal (the guest still dies of it). `ACOSART_CRASH_DUMP=0` disables.
        try {
            int crash = Nat.installCrashHandler();
            System.out.println("ACOSLAUNCH MARK crash-handler rc=" + crash);
        } catch (Throwable t) {
            System.out.println("ACOSLAUNCH WARN crash-handler unavailable " + t);
        }
        // P12: install the session animation scale before any framework animator
        // can start (before Activity attach and the app's first traversal).
        applyAnimations(sAnimationsRequested, sAnimationsRequestSource);
        // W24: the device's static RROs, before any framework `Resources` exists.
        applySystemOverlays();
        int looper = Nat.startBinderLooper();
        if (looper != 1) throw new IllegalStateException("startBinderLooper -> " + looper);
        System.out.println("ACOSLAUNCH MARK binder-looper started");
        registerCameraFrameSink();

        Class<?> at = Class.forName("android.app.ActivityThread");
        Constructor<?> ctor = at.getDeclaredConstructor();
        ctor.setAccessible(true);
        Object thread = ctor.newInstance();
        Field fAppThread = at.getDeclaredField("mAppThread");
        fAppThread.setAccessible(true);
        IBinder appThread = (IBinder) fAppThread.get(thread);
        if (appThread == null) throw new IllegalStateException("ActivityThread.mAppThread == null");
        // W38: keep the real `IApplicationThread` interface object and the
        // ActivityThread so the AMS stand-in can deliver stock
        // `ClientTransaction`s in-process (with real `Binder` activity tokens,
        // which `Parcel.marshall()` refuses to carry over the peer byte pipe).
        sThread = thread;
        sClient = iatClient(appThread);
        byte[] bind = buildBindApplicationRequest();
        pushToPeer(1, bind);
        System.out.println("ACOSLAUNCH MARK bind-request-pushed bytes=" + bind.length);
        byte[] display = buildDisplayInfoReply();
        pushReplyTemplate("android.hardware.display.IDisplayManager", 1, display);
        System.out.println("ACOSLAUNCH MARK displayinfo-reply-pushed bytes=" + display.length);
        // MPROC1: `IPackageManager.getApplicationInfo` (code 9) is **not**
        // answered from a guest-built template any more. The template ignored
        // the request's package argument, so with a second package installed
        // `getApplicationInfo("other.package")` (and therefore
        // `createPackageContext`, `LoadedApk` for that package, and every
        // per-package resolver) returned the launched app's own
        // `ApplicationInfo`. The host package manager answers code 9 for the
        // package the caller asked for, and its codec is the API-34
        // `ApplicationInfo.writeToParcel` layout (proved by the multi-package
        // probe + `scenarios/mproc1-*.yaml`).
        byte[] instr = buildInstrumentationInfoReply();
        // `getInstrumentationInfoAsUser(ComponentName, int, int)` = 44 on API 34.
        pushReplyTemplate("android.content.pm.IPackageManager", 44, instr);
        System.out.println("ACOSLAUNCH MARK instrumentationinfo-reply-pushed bytes=" + instr.length);

        Method attach = at.getDeclaredMethod("attach", boolean.class, long.class);
        attach.setAccessible(true);
        long seq = System.currentTimeMillis();
        try {
            attach.invoke(thread, false, seq);
        } catch (InvocationTargetException e) {
            throw e.getCause() != null ? e.getCause() : e;
        }
        System.out.println("ACOSLAUNCH MARK attach-returned startSeq=" + seq);
        if (System.getenv("ACOSLAUNCH_PROBE") != null) probeDisplayInfoParcel();

        // The framework's own main-thread message tracing (`Looper.setMessageLogging`,
        // the facility AMS and `Looper`-based tools use): every dispatched message
        // is printed with its handler and `what`, so "110 = BIND_APPLICATION" and
        // "159 = EXECUTE_TRANSACTION" start/finish lines are the logcat-equivalent
        // proof that the real `handleBindApplication` / `TransactionExecutor` ran
        // to completion on the main thread.
        installLooperTrace();
        // W27 item 3: a second-Activity start currently reaches
        // IActivityTaskManager.startActivity, whose Intent parcelable codec is
        // owned by another component and rejects the request. Rather than let
        // the binder exception kill the guest (W22's "guest ended after the
        // gesture"), install a guest-side guard that returns START_SUCCESS and
        // publishes a structured UNSUPPORTED_ANDROID_API marker naming the call.
        // The app keeps running; no state is fabricated.
        installStartActivityGuard();

        // The bindApplication H message is already posted; run the real main
        // looper. The watcher acts as the AMS side of the protocol: it waits for
        // the real `Application` to exist, then (for activity-resumed) sends the
        // one ClientTransaction AMS would send.
        startWatcher(thread, at, appThread);
        // MPROC1: the scripted activity-result watcher. The agent scripts the
        // answer a share sheet / picker would give (session control op
        // `acos_service_ctl {module:"intents", op:"result"}`); the launcher
        // writes it to this run's own `/data/out/launch/activity-result.json`
        // and this thread delivers it as the framework's own
        // `ActivityResultItem` transaction, acking in `activity-result.ack`.
        startResultWatcher();
        // Frame-clock probe (L1): post a real `Choreographer` frame callback on
        // the main thread *before* the looper runs. `Choreographer.getInstance()`
        // builds the `DisplayEventReceiver` (ISurfaceComposer →
        // createDisplayEventConnection → stealReceiveChannel → BitTube) and
        // registers the BitTube's receive fd with the main `Looper`; the callback
        // can only fire if a 216-byte vsync event actually arrives on that fd from
        // the Rust clock, and `Choreographer$FrameData.update` also calls
        // `getLatestVsyncEventData` (the parcelable reply), so a frame proves both
        // halves. This is the same receiver instance `ViewRootImpl`'s constructor
        // gets (Choreographer is a ThreadLocal). Off by default.
        // `launch.rs`'s sandbox clears the environment and re-seeds a fixed
        // allowlist (`ACOSLAUNCH_PROBE`, not `ACOSLAUNCH_FRAME_PROBE`), so both
        // spellings enable the diagnostic probe. The frame probe's own name is
        // preferred; `ACOSLAUNCH_PROBE` is the one the launcher actually forwards.
        if (System.getenv("ACOSLAUNCH_FRAME_PROBE") != null
                || System.getenv("ACOSLAUNCH_PROBE") != null) {
            postFrameProbe();
        }
        try {
            Looper.loop();
        } catch (Throwable fatal) {
            // The real main looper stops at the first uncaught exception, exactly
            // as `AndroidRuntime` does. The lifecycle state the framework had
            // already reached is still readable, so record it before exiting:
            // `handleResumeActivity` runs `performResumeActivity` (Activity.onResume)
            // *before* `WindowManagerGlobal.addView`, so a window-path throw does
            // not undo the resume.
            boolean resumed = recordResumeAfterLooperFailure(thread, at, fatal);
            // The stack is the diagnosis of the *next* wall: print it before the
            // one-line outcome so a `--until activity-resumed` run that got past
            // the resume still names what stopped it.
            unwrap(fatal).printStackTrace(System.out);
            System.out.println("ACOSLAUNCH {\"outcome\":\"crash\",\"code\":\"LOOPER_THREW\","
                    + "\"activity_resumed\":" + resumed + ",\"message\":\""
                    + esc(String.valueOf(unwrap(fatal))) + "\""
                    + crashDetail(unwrap(fatal)) + "}");
            System.out.flush();
            System.exit(1);
            return;
        }

        long wall = System.currentTimeMillis() - t0;
        if (!sApplicationCreated) {
            System.out.println("ACOSLAUNCH {\"outcome\":\"crash\",\"code\":\"NO_APPLICATION\"}");
            System.exit(1);
        }
        String result = "{\"outcome\":\"ok\",\"package\":\"" + esc(sPkg) + "\",\"apk\":\""
                + esc(sApk) + "\",\"application_created\":true,\"until\":\"" + esc(sUntil)
                + "\",\"activity_launched\":" + (sActivityResumed ? "true" : "false")
                + ",\"finish_during_create\":" + (sFinishDuringCreate ? "true" : "false")
                + ",\"wall_ms\":" + wall + "}";
        write(new File(sOut, "launch.json"), result);
        System.out.println("ACOSLAUNCH " + result);
        System.out.flush();
        // Dalvik's main thread has quit its looper; exit explicitly so the
        // process does not wait for non-daemon binder threads.
        System.exit(0);
    }

    /**
     * W5 diagnostic: why the first traversal did not draw. Prints the real
     * `ViewRootImpl` state after the hold (surface validity, visibility, first
     * traversal, display state, BLAST queue, dirty rect). Nothing is modified.
     */
    static void probeViewRoot(Object activity) {
        try {
            Object window = activity.getClass().getMethod("getWindow").invoke(activity);
            Object decor = window.getClass().getMethod("getDecorView").invoke(window);
            java.lang.reflect.Method m = Class.forName("android.view.View")
                    .getMethod("getViewRootImpl");
            Object vr = m.invoke(decor);
            if (vr == null) {
                System.out.println("ACOSLAUNCH MARK viewroot-probe: no ViewRootImpl");
                return;
            }
            // Field reads only. W6: `Surface.isValid()` takes the Java
            // `Surface` monitor that `Surface.lockCanvas` holds across
            // `nativeLockCanvas`, so probing it here deadlocked the watcher
            // against a draw that is in flight (ART dump in
            // results/art-runtime/W6.md §3). `mSurface != null` and
            // `isValid` are equivalent for this diagnostic.
            Object surface = get(vr, "mSurface");
            Object attach = get(vr, "mAttachInfo");
            Object view = get(vr, "mView");
            Object dirty = get(vr, "mDirty");
            // `getHostVisibility()` is the value `performTraversals` gates
            // `performDraw` on (`mAppVisible ? mView.getVisibility() : GONE`).
            // `mViewVisibility` is only the value stored at the *end* of a
            // traversal, so it stays at its constructor default (GONE) while a
            // draw is in flight even though the window is visible.
            Object hostVisibility = "<n/a>";
            try {
                java.lang.reflect.Method hv = vr.getClass().getDeclaredMethod("getHostVisibility");
                hv.setAccessible(true);
                hostVisibility = hv.invoke(vr);
            } catch (Throwable t) {
                hostVisibility = "<" + t + ">";
            }
            System.out.println("ACOSLAUNCH MARK viewroot-probe surface_valid="
                    + (surface != null)
                    + " view_visibility=" + get(vr, "mViewVisibility")
                    + " host_visibility=" + hostVisibility
                    + " app_visible=" + get(vr, "mAppVisible")
                    + " decor_visibility=" + (view == null ? "null" : get(view, "mViewFlags"))
                    + " first=" + get(vr, "mFirst")
                    + " display_state=" + (attach == null ? "null" : get(attach, "mDisplayState"))
                    + " renderer=" + (attach == null ? "null" : get(attach, "mThreadedRenderer"))
                    + " dirty=" + dirty
                    + " layout_requested=" + get(vr, "mLayoutRequested")
                    + " last_touch=" + get(vr, "mLastTouchPoint")
                    + " touch_source=" + get(vr, "mLastTouchSource")
                    + " touch_device=" + get(vr, "mLastTouchDeviceId")
                    + " pending_input=" + get(vr, "mPendingInputEventCount")
                    + " in_touch_mode=" + (attach == null ? "null" : get(attach, "mInTouchMode"))
                    + " has_focus=" + (attach == null ? "null" : get(attach, "mHasWindowFocus")));
        } catch (Throwable x) {
            System.out.println("ACOSLAUNCH MARK viewroot-probe failed " + x);
        }
    }

    /**
     * I2 read-only input probe: walks the live decor view and prints the
     * counter's text plus the Increment button's window-local bounds (the
     * coordinate space an `InputChannel` MotionEvent uses). Nothing is
     * touched or invoked; this only reads layout state.
     */
    static void probeInputTree(Object activity) {
        try {
            Object window = activity.getClass().getMethod("getWindow").invoke(activity);
            Object decor = window.getClass().getMethod("getDecorView").invoke(window);
            if (!(decor instanceof View)) {
                System.out.println("ACOSLAUNCH MARK input-probe: no decor view");
                return;
            }
            StringBuilder tree = new StringBuilder();
            walkViews((View) decor, 0, tree);
            System.out.println("ACOSLAUNCH MARK input-probe count_text=" + sCountText
                    + " inc_bounds=" + sIncBounds + " tree={" + tree + "}");
        } catch (Throwable x) {
            System.out.println("ACOSLAUNCH MARK input-probe failed " + x);
        }
    }

    static String sCountText = "<none>";
    static String sIncBounds = "<none>";

    /**
     * W23: only ask the resource table for names of ids that can carry a
     * package id.
     *
     * <p>Android resource ids are {@code 0xPPTTEEEE} (package/type/entry); a
     * package byte of 0 is not a resource id at all. Runtime ids produced by
     * {@link View#generateViewId()} start at 1 and stay below 0x01000000, and
     * React Native tags every view it creates with one. Resolving those through
     * {@code Resources} makes libandroidfw log one ERROR per lookup
     * (`AssetManager2::FindEntry`: "Invalid resource ID 0x%08x."), so the probe
     * itself produced ~14 spurious error lines per 50 ms sample of an RN app
     * (W23 measured 7 generated ids x 2 lookups x 327 samples = 4578 lines) and
     * they looked like an app failure in every log review. On a device the same
     * call throws {@code NotFoundException}, which is why the probe already
     * treated it as "no name"; skipping the lookup keeps the tree identical and
     * stops the probe from writing ERROR-level lines it did not earn.
     */
    static boolean isResourceId(int id) {
        return id != View.NO_ID && (id >>> 24) != 0;
    }

    static void walkViews(View v, int depth, StringBuilder tree) {
        try {
            String idName = "";
            try {
                int id = v.getId();
                if (isResourceId(id)) {
                    idName = v.getResources().getResourceEntryName(id);
                }
            } catch (Throwable ignored) {
            }
            String text = "";
            if (v instanceof TextView) {
                CharSequence cs = ((TextView) v).getText();
                text = cs == null ? "" : cs.toString();
            }
            int[] loc = new int[2];
            v.getLocationInWindow(loc);
            String row = depth + ":" + v.getClass().getSimpleName()
                    + (idName.isEmpty() ? "" : "#" + idName)
                    + (text.isEmpty() ? "" : "='" + text + "'")
                    + "@" + v.getWidth() + "x" + v.getHeight()
                    + "+" + loc[0] + "+" + loc[1]
                    + (v.getVisibility() == View.VISIBLE ? "" : "(vis=" + v.getVisibility() + ")");
            if (tree.length() < 3000) {
                if (tree.length() > 0) tree.append(' ');
                tree.append(row);
            }
            if ("count".equals(idName)) sCountText = text;
            if ("inc".equals(idName)) {
                sIncBounds = "[" + loc[0] + "," + loc[1] + " - "
                        + (loc[0] + v.getWidth()) + "," + (loc[1] + v.getHeight()) + "]"
                        + " center=" + (loc[0] + v.getWidth() / 2) + ","
                        + (loc[1] + v.getHeight() / 2);
            }
            if (v instanceof ViewGroup) {
                ViewGroup g = (ViewGroup) v;
                for (int i = 0; i < g.getChildCount(); i++) {
                    walkViews(g.getChildAt(i), depth + 1, tree);
                }
            }
        } catch (Throwable ignored) {
        }
    }

    // ------------------------------------------------------- live UI probe (W10)

    /** P3b: wakeup for the probe thread, so a host request is not a 50 ms wait. */
    static final Object sProbeWake = new Object();
    static boolean sProbeRequested = false;
    /**
     * SETTLE5: the probe channel's own liveness facts, so a host that waits for
     * a sample can never be left guessing whether its wake reached the guest.
     * `sProbePostedMs` is when the probe thread handed a sample to the main
     * looper, `sProbePending` is whether that runnable is still queued (the main
     * thread is busy or blocked), and `sLastWakeMarkMs` rate-limits the MARK.
     */
    static volatile long sProbePostedMs = 0;
    static volatile boolean sProbePending = false;
    static volatile long sLastWakeMarkMs = 0;
    /**
     * SETTLE5: the host's demand channel is this control thread. Once it has
     * ended (stdin EOF/close, a dead reader), no `PROBE` wake can ever arrive
     * again, so the 5 s idle fallback would be the only sample source and every
     * strict settle (1.5 s deadline) would time out on a *healthy, idle*
     * screen. A dead control channel switches the fallback to a fast poll: the
     * sample is still a real probe sample with the same settle rule, so this can
     * never turn a stale observation into a pass — it only keeps the screen
     * observable without the demand wake.
     */
    static volatile boolean sProbeControlDead = false;

    // ------------------------------------------------- P10: view-dirty probe
    //
    // P3 measured ~19.4 full tree walks/s on an idle screen because the probe
    // thread polled every 50 ms. P10 makes the walk event-driven: a listener on
    // every attached root marks the tree dirty (and bumps a generation) on
    // layout, draw, window attach/detach and focus change. Listeners never walk
    // and never serialise; they only set a flag and wake the probe thread. The
    // thread emits at most one sample per debounce window (one frame), and a
    // slow poll remains the fallback so a missed event can only delay a sample,
    // never stop it (canvases / draws we do not observe).
    static final java.util.concurrent.atomic.AtomicBoolean sTreeDirty =
            new java.util.concurrent.atomic.AtomicBoolean(false);
    static volatile long sDirtyAtMs = 0;
    static volatile long sViewGeneration = 0;
    /** Roots whose ViewTreeObserver already has our listeners (identity map). */
    static final java.util.IdentityHashMap<View, Boolean> sWatchedRoots =
            new java.util.IdentityHashMap<View, Boolean>();

    // --------------------------------------------- P12: agent animations flag
    //
    // Real Android's `animator_duration_scale` reaches the framework as
    // `ValueAnimator.setDurationScale` (WMS `getCurrentAnimatorScale` at
    // `WindowManagerGlobal` init, then `IWindowSessionCallback` on changes).
    // ACOS has no SettingsProvider, so the launcher (the WMS host) reports the
    // same scale and the guest installs it at the same framework point. This
    // is the scale every ValueAnimator/ObjectAnimator/StateListAnimator and the
    // native RenderNodeAnimator read; transition/window chrome on a device is
    // WMS-side and ACOS's WMS runs no transition animations.
    static volatile boolean sAnimationsOn = true;
    static volatile float sAnimationsScale = 1.0f;
    static volatile boolean sAnimationsApplied = false;
    static volatile String sAnimationsSource = "default";

    static void applyAnimations(boolean on, String source) {
        float scale = on ? 1.0f : 0.0f;
        boolean applied = false;
        try {
            Class<?> va = Class.forName("android.animation.ValueAnimator");
            Method set = va.getMethod("setDurationScale", float.class);
            set.invoke(null, scale);
            try {
                Object read = va.getMethod("getDurationScale").invoke(null);
                if (read instanceof Number) scale = ((Number) read).floatValue();
            } catch (Throwable ignored) {
            }
            applied = true;
        } catch (Throwable t) {
            System.out.println("ACOSLAUNCH WARN animations apply failed: "
                    + unwrap(t));
        }
        sAnimationsOn = on;
        sAnimationsScale = scale;
        sAnimationsApplied = applied;
        sAnimationsSource = source;
        System.out.println("ACOSLAUNCH MARK animations on=" + on + " scale=" + scale
                + " applied=" + applied + " source=" + source);
        System.out.flush();
    }

    // ------------------------------------------------ P11: event-strict settle
    //
    // Strict settle is event-only (P12, owner decision 2026-11-12). The guest
    // reports whether the framework's own UI work is finished, so the host can
    // bind a settled observation without a fixed delay.
    // The checks are read-only field reads / getters on the UI thread.
    static volatile Object sLastLiveActivity;
    static volatile long sLastDrawMs = 0;
    static volatile long sDrawSeq = 0;
    static volatile String sWindowSignature = "";
    static volatile long sWindowSignatureAtMs = 0;
    static long sSettleHorizonMs = -1;

    static long settleHorizonMs() {
        if (sSettleHorizonMs < 0) {
            sSettleHorizonMs = envLong("ACOS_STRICT_SETTLE_HORIZON_MS", 256);
        }
        return sSettleHorizonMs;
    }

    /** P11: what the framework says about pending UI work at this sample. */
    static final class SettleSignals {
        boolean settled;
        final ArrayList<String> pending = new ArrayList<String>();
        int frameCallbacks;
        /** H15: ANIMATION-queue callback class names (P10_PROBE_ANIM_TRACE only). */
        final ArrayList<String> animCallbacks = new ArrayList<String>();
        long queueDueMs = -1;
        long horizonMs;
        long windowStableMs;
        /** H15: the window's touch-mode answer when this sample was built. */
        boolean inTouchMode;
        /** ROT8: the app's last window is gone and every Activity is destroyed. */
        boolean windowGone;

        void add(String reason) {
            for (int i = 0; i < pending.size(); i++) {
                if (pending.get(i).equals(reason)) return;
            }
            if (pending.size() < 12) pending.add(reason);
        }
    }

    /**
     * H15 diagnostic (P10_PROBE_ANIM_TRACE=1 only): name the running framework
     * animators. `AnimationHandler$1` is the animator *pump*; the animator that
     * keeps drawing a relaunched Activity is one of the `AnimationHandler`
     * callbacks, so read that list and describe each entry (`ValueAnimator`
     * duration/target/property). Read-only reflection, never throws.
     */
    static String animationHandlerDump() {
        try {
            Class<?> ah = Class.forName("android.animation.AnimationHandler");
            Object inst = ah.getMethod("getInstance").invoke(null);
            Object local = staticField(ah, "mAnimationCallbacks");
            Object callbacks = local instanceof ThreadLocal
                    ? ((ThreadLocal<?>) local).get() : get(inst, "mAnimationCallbacks");
            if (!(callbacks instanceof java.util.List)) return null;
            java.util.List<?> list = (java.util.List<?>) callbacks;
            StringBuilder b = new StringBuilder();
            for (int i = 0; i < list.size() && i < 4; i++) {
                Object cb = list.get(i);
                if (cb == null) continue;
                if (b.length() > 0) b.append(';');
                b.append(cb.getClass().getSimpleName());
                Object dur = get(cb, "mDuration");
                if (dur instanceof Number) b.append(" dur=").append(dur);
                Object prop = get(cb, "mPropertyName");
                if (prop != null) b.append(" prop=").append(prop);
                Object target = get(cb, "mTarget");
                if (target instanceof java.lang.ref.Reference) {
                    Object view = ((java.lang.ref.Reference<?>) target).get();
                    b.append(" target=").append(view == null ? "null" : view.getClass().getName());
                }
                Object props = get(cb, "mValues");
                if (props instanceof Object[]) b.append(" values=").append(((Object[]) props).length);
                Object startDelay = get(cb, "mStartDelay");
                if (startDelay instanceof Number) b.append(" delay=").append(startDelay);
                for (String listName : new String[]{"mUpdateListeners", "mListeners"}) {
                    Object list2 = get(cb, listName);
                    if (!(list2 instanceof java.util.List)) continue;
                    java.util.List<?> ls = (java.util.List<?>) list2;
                    b.append(' ').append(listName).append("=[");
                    for (int k = 0; k < ls.size() && k < 3; k++) {
                        Object l = ls.get(k);
                        if (k > 0) b.append(',');
                        b.append(l == null ? "null" : l.getClass().getName());
                    }
                    b.append(']');
                }
            }
            return b.length() == 0 ? null : b.toString();
        } catch (Throwable ignored) {
            return null;
        }
    }

    /** H15: the ANIMATION-queue callback names, JSON (empty unless tracing). */
    static String animCallbacksJson(SettleSignals s) {
        StringBuilder b = new StringBuilder();
        for (int i = 0; i < s.animCallbacks.size(); i++) {
            if (i > 0) b.append(',');
            b.append('"').append(jesc(s.animCallbacks.get(i))).append('"');
        }
        return b.toString();
    }

    static void addPending(SettleSignals s, String reason) {
        s.add(reason);
    }

    static Object staticField(Class<?> cls, String name) {
        try {
            java.lang.reflect.Field f = findField(cls, name);
            f.setAccessible(true);
            return f.get(null);
        } catch (Throwable ignored) {
            return null;
        }
    }

    static int collectionSize(Object o) {
        if (o instanceof java.util.Collection) return ((java.util.Collection<?>) o).size();
        if (o instanceof java.util.Map) return ((java.util.Map<?, ?>) o).size();
        return 0;
    }

    /** ViewRootImpl for a window root (its parent), read-only. */
    static Object viewRootOf(View root) {
        try {
            Object parent = root.getParent();
            if (parent != null && "android.view.ViewRootImpl".equals(parent.getClass().getName())) {
                return parent;
            }
        } catch (Throwable ignored) {
        }
        Object attach = get(root, "mAttachInfo");
        return get(attach, "mViewRootImpl");
    }

    /** A read-only digest of window/focus/IME state; a change restarts stability. */
    static String windowSignature(java.util.List<View> roots, View decor) {
        StringBuilder b = new StringBuilder(128);
        b.append(roots == null ? 0 : roots.size());
        if (roots != null) {
            for (int i = 0; i < roots.size(); i++) {
                View root = roots.get(i);
                b.append('|').append(i).append(':');
                if (root == null) {
                    b.append("null");
                    continue;
                }
                try {
                    b.append(root == decor ? "main" : windowLabel(root, i));
                } catch (Throwable ignored) {
                    b.append('?');
                }
                Object vr = viewRootOf(root);
                Object ai = get(vr, "mAttachInfo");
                b.append(",f=").append(get(ai, "mHasWindowFocus"));
                Object ime = get(vr, "mImeFocusController");
                b.append(",i=").append(get(ime, "mHasImeFocus"));
                Object insets = get(ai, "mVisibleInsets");
                b.append(",v=").append(String.valueOf(insets));
            }
        }
        return b.toString();
    }

    /** Check every View for layout/animation work the tree itself must finish. */
    static void checkViewAnimations(View v, SettleSignals s) {
        if (v == null) return;
        try {
            android.view.animation.Animation a = v.getAnimation();
            if (a != null && !a.hasEnded()) addPending(s, "anim");
        } catch (Throwable ignored) {
        }
        try {
            Object vpa = get(v, "mAnimator");
            Object map = get(vpa, "mAnimatorMap");
            if (map instanceof java.util.Map && !((java.util.Map<?, ?>) map).isEmpty()) {
                addPending(s, "anim");
            }
        } catch (Throwable ignored) {
        }
        if (v instanceof ViewGroup) {
            try {
                android.animation.LayoutTransition lt =
                        ((ViewGroup) v).getLayoutTransition();
                if (lt != null && lt.isRunning()) addPending(s, "anim");
            } catch (Throwable ignored) {
            }
            ViewGroup g = (ViewGroup) v;
            try {
                int n = g.getChildCount();
                for (int i = 0; i < n; i++) checkViewAnimations(g.getChildAt(i), s);
            } catch (Throwable ignored) {
            }
        }
    }

    /** P11: all framework signals that make a sample settled (read-only, UI thread). */
    static SettleSignals checkSettle(java.util.List<View> roots, View decor) {
        SettleSignals s = new SettleSignals();
        s.horizonMs = settleHorizonMs();
        if (roots == null || roots.isEmpty()) addPending(s, "no-window");
        boolean anyFocus = false;
        if (roots != null) {
            for (int i = 0; i < roots.size(); i++) {
                View root = roots.get(i);
                if (root == null) {
                    addPending(s, "no-window");
                    continue;
                }
                Object vr = viewRootOf(root);
                if (vr == null) {
                    addPending(s, "detached");
                    continue;
                }
                // A stopped/removed window is not part of the current visible
                // state (an Activity transition leaves the old root in
                // WindowManagerGlobal briefly). Ignore it; the live root is
                // still checked below, and `no-focus` covers "no live window".
                if (Boolean.FALSE.equals(get(vr, "mAdded"))) continue;
                if (Boolean.TRUE.equals(get(vr, "mStopped"))) continue;
                if (Boolean.TRUE.equals(get(vr, "mPausedForTransition"))) {
                    addPending(s, "window-paused");
                }
                if (Boolean.TRUE.equals(get(vr, "mIsInTraversal"))) addPending(s, "traversal");
                if (Boolean.TRUE.equals(get(vr, "mTraversalScheduled"))) {
                    addPending(s, "traversal");
                }
                if (Boolean.TRUE.equals(get(vr, "mLayoutRequested"))) {
                    addPending(s, "layout-vr");
                }
                if (root.isLayoutRequested()) addPending(s, "layout-root");
                Object dirty = get(vr, "mDirty");
                if (dirty instanceof android.graphics.Rect
                        && !((android.graphics.Rect) dirty).isEmpty()) {
                    addPending(s, "dirty");
                }
                Object transitions = get(vr, "mPendingTransitions");
                if (collectionSize(transitions) > 0) addPending(s, "transition");
                Object ai = get(vr, "mAttachInfo");
                if (Boolean.TRUE.equals(get(ai, "mHasWindowFocus"))) anyFocus = true;
                checkViewAnimations(root, s);
            }
        }
        if (!anyFocus) addPending(s, "no-focus");

        // Choreographer: pending INPUT/ANIMATION/TRAVERSAL/INSETS callbacks.
        try {
            Object chore = android.view.Choreographer.getInstance();
            Object queues = get(chore, "mCallbackQueues");
            if (queues instanceof Object[]) {
                Object[] qs = (Object[]) queues;
                for (int i = 0; i < qs.length; i++) {
                    if (get(qs[i], "mHead") == null) continue;
                    s.frameCallbacks++;
                    // CALLBACK_ANIMATION (1) is the framework's global animator
                    // pump. The host's draw_seq confirmation is the rendered
                    // state signal: an app animation that draws changes the
                    // draw sequence every frame, so it is still waited for. A
                    // Button press/elevation ValueAnimator here would otherwise
                    // pin every tap to ~1 s on this image. INPUT/TRAVERSAL/insets
                    // callbacks are still hard gates.
                    if (i != 1) addPending(s, "frame");
                    // H15 diagnostic (only with P10_PROBE_ANIM_TRACE=1): name the
                    // ANIMATION-queue callbacks that keep drawing, so a post-launch
                    // animation can be attributed to a class instead of guessed.
                    if (i == 1 && sAnimTrace) {
                        Object head = get(qs[i], "mHead");
                        Object cb = get(head, "action");
                        if (cb == null) cb = get(head, "mCallback");
                        String name = cb == null ? "null" : cb.getClass().getName();
                        if (s.animCallbacks.size() < 3) s.animCallbacks.add(name);
                        String detail = animationHandlerDump();
                        if (detail != null && s.animCallbacks.size() < 3) {
                            s.animCallbacks.add(detail);
                        }
                    }
                }
            }
            // `frame` is added per non-animation queue above (see the note).
        } catch (Throwable ignored) {
        }

        // Main Looper: no message due inside the horizon (postDelayed updates).
        try {
            Object mq = Looper.getMainLooper().getQueue();
            Object msg = get(mq, "mMessages");
            long now = android.os.SystemClock.uptimeMillis();
            if (msg != null) {
                Object when = get(msg, "when");
                if (when instanceof Number) {
                    s.queueDueMs = ((Number) when).longValue() - now;
                    Object callback = get(msg, "callback");
                    String callbackClass = callback == null ? "null" : callback.getClass().getName();
                    // The scrollbar fade is framework chrome (View.ScrollabilityCache),
                    // not app state: its 1.2 s timer would otherwise pin every
                    // scrollable screen to ~1.2 s. Quiet strict also returns before
                    // it runs, so ignoring it keeps the two rules comparable.
                    boolean cosmetic = "android.view.View$ScrollabilityCache"
                            .equals(callbackClass);
                    if (!cosmetic && s.queueDueMs <= s.horizonMs) addPending(s, "queue");
                }
            }
        } catch (Throwable ignored) {
        }

        // Window/focus/IME state must have been stable for one frame.
        long now = System.currentTimeMillis();
        String sig = windowSignature(roots, decor);
        if (!sig.equals(sWindowSignature)) {
            sWindowSignature = sig;
            sWindowSignatureAtMs = now;
        }
        s.windowStableMs = now - sWindowSignatureAtMs;
        try {
            s.inTouchMode = decor.isInTouchMode();
        } catch (Throwable ignored) {
        }
        if (s.windowStableMs < 16) addPending(s, "window-unstable");

        s.settled = s.pending.isEmpty();
        return s;
    }

    /** Mark the view tree dirty. Called from view callbacks; must never throw. */
    static void markTreeDirty() {
        sViewGeneration++;
        sDirtyAtMs = System.currentTimeMillis();
        sTreeDirty.set(true);
        synchronized (sProbeWake) {
            sProbeWake.notifyAll();
        }
    }

    /** Register the dirty listeners on one window root, once. */
    static void watchRoot(final View root) {
        if (root == null) return;
        try {
            synchronized (sWatchedRoots) {
                if (sWatchedRoots.containsKey(root)) return;
            }
            final android.view.ViewTreeObserver vto = root.getViewTreeObserver();
            if (vto == null || !vto.isAlive()) return;
            vto.addOnGlobalLayoutListener(
                    new android.view.ViewTreeObserver.OnGlobalLayoutListener() {
                        @Override
                        public void onGlobalLayout() {
                            markTreeDirty();
                        }
                    });
            vto.addOnDrawListener(new android.view.ViewTreeObserver.OnDrawListener() {
                @Override
                public void onDraw() {
                    // P11: the host binds the settled sample to a draw, so the
                    // draw's own wall clock travels with every event-mode sample.
                    sLastDrawMs = System.currentTimeMillis();
                    sDrawSeq++;
                    markTreeDirty();
                }
            });
            vto.addOnWindowAttachListener(
                    new android.view.ViewTreeObserver.OnWindowAttachListener() {
                        @Override
                        public void onWindowAttached() {
                            markTreeDirty();
                        }

                        @Override
                        public void onWindowDetached() {
                            markTreeDirty();
                        }
                    });
            vto.addOnWindowFocusChangeListener(
                    new android.view.ViewTreeObserver.OnWindowFocusChangeListener() {
                        @Override
                        public void onWindowFocusChanged(boolean hasFocus) {
                            markTreeDirty();
                        }
                    });
            synchronized (sWatchedRoots) {
                sWatchedRoots.put(root, Boolean.TRUE);
            }
        } catch (Throwable ignored) {
            // A listener we cannot register is covered by the fallback poll.
        }
    }

    /** Watch every current window root (main + dialogs/menus/popups). */
    static void watchProbeRoots(View decor) {
        try {
            watchRoot(decor);
            java.util.List<View> roots = windowViews();
            if (roots != null) {
                for (View root : roots) watchRoot(root);
            }
        } catch (Throwable ignored) {
            // Fallback poll still covers an unobserved root.
        }
    }

    /**
     * W10: a periodic read-only view-tree sample for the interactive session.
     *
     * <p>The walk runs on the main thread (a Handler post between messages), so
     * it observes the same state the app draws and never races a traversal.
     * Every sample is one stdout line: {@code ACOSPROBE } followed by JSON
     * {@code {"seq":N,"time":ms,"tree":{"nodes":[...],"node_budget":N,
     * "truncated":bool}}}. W31: a sample that stopped at the {@link
     * #PROBE_NODE_BUDGET} cap says so; the engine copies the flag into the
     * saved {@code UiTree}, so a saved tree never claims completeness it does
     * not have.
     *
     * <p>Bounds are reported twice: {@code bounds} is
     * {@code View.getLocationInWindow} (the coordinate space the host's
     * InputChannel MotionEvents use, I2) and {@code bounds_on_screen} is
     * {@code View.getLocationOnScreen} (the display space the reference
     * scenarios and the Rust VM tree use). The engine chooses; both come from
     * stock View getters and are never inferred.
     *
     * <p>{@code text} is {@code TextView.getText()}, falling back to
     * {@code getHint()} when the field is empty (uiautomator-compatible, and
     * the W15 fix for the four fixtures that assert an EditText's hint).
     * {@code hint} always carries the hint itself, so a consumer that wants
     * the literal text is not forced through the fallback. Nothing is
     * invoked; the only calls are getters.
     *
     * <p>P10: the walk is event-driven. A view callback marks the tree dirty;
     * the probe thread emits one sample per debounce window (one frame) and
     * coalesces bursts, so a burst of draws never becomes a burst of walks.
     * The slow poll (`P10_PROBE_FALLBACK_MS`, default 5000 ms; H4) is the
     * liveness fallback: a missed event can delay a sample but can never stop
     * it. A host `PROBE` line on stdin schedules one sample immediately (cause
     * {@code wake}), so every host settle/tree request still gets a fresh
     * sample without waiting for this timer. H4 raised the default from 250 ms:
     * an idle screen was doing four full tree walks/s for no observer.
     * Every sample names its {@code cause}: {@code dirty|wake|poll}.
     */
    /**
     * R2 W6: geometry trace. Every place the app can read a display or window
     * size is sampled from a watcher thread and printed whenever it changes:
     * `DisplayMetrics`, `Configuration.screenWidthDp/HeightDp`, the
     * `WindowManager` window metrics (bounds + insets), the decor view's laid
     * out size and screen position, and `ViewRootImpl.mWinFrame` / `mWidth` /
     * `mHeight` (what the last `relayout` reply told it).
     *
     * <p>Off unless `ACOSLAUNCH_GEOM_TRACE` is forwarded. Read-only: reflection
     * field reads of scalars and the framework's own getters, no mutation, no
     * `Surface` monitor (see the W6 deadlock note in {@link #probeViewRoot}).
     */
    static void startGeometryTrace(final Object activity) {
        final long t0 = System.currentTimeMillis();
        final java.io.File out = new java.io.File(sOut, "geometry-trace.txt");
        Thread t = new Thread(new Runnable() {
            @Override
            public void run() {
                String last = "";
                for (int i = 0; i < 60; i++) {
                    String line = geometryLine(activity, System.currentTimeMillis() - t0);
                    if (!line.equals(last)) {
                        System.out.println(line);
                        System.out.flush();
                        try {
                            appendLine(out, line);
                        } catch (Throwable ignored) {
                        }
                        last = line;
                    }
                    try {
                        Thread.sleep(100);
                    } catch (Throwable ignored) {
                        return;
                    }
                }
                try {
                    appendLine(out, "ACOSGEO trace-end elapsed_ms="
                            + (System.currentTimeMillis() - t0));
                } catch (Throwable ignored) {
                }
            }
        }, "acos-geometry-trace");
        t.setDaemon(true);
        t.start();
    }

    static void appendLine(java.io.File f, String line) throws Exception {
        java.io.FileOutputStream fos = new java.io.FileOutputStream(f, true);
        try {
            fos.write((line + "\n").getBytes("UTF-8"));
        } finally {
            fos.close();
        }
    }

    /** One `ACOSGEO` sample: every size the app can observe, or `<err>` per field. */
    static String geometryLine(final Object activity, long elapsedMs) {
        StringBuilder b = new StringBuilder("ACOSGEO t=").append(elapsedMs);
        try {
            android.content.res.Resources res = (android.content.res.Resources)
                    activity.getClass().getMethod("getResources").invoke(activity);
            android.util.DisplayMetrics dm = res.getDisplayMetrics();
            android.content.res.Configuration cfg = res.getConfiguration();
            b.append(" app_dm=").append(dm.widthPixels).append('x').append(dm.heightPixels)
                    .append('@').append(dm.densityDpi);
            b.append(" app_cfg=").append(cfg.screenWidthDp).append('x').append(cfg.screenHeightDp)
                    .append("@").append(cfg.densityDpi)
                    .append(" smallest=").append(cfg.smallestScreenWidthDp)
                    .append(" orient=").append(cfg.orientation);
            android.util.DisplayMetrics sdm = android.content.res.Resources.getSystem()
                    .getDisplayMetrics();
            android.content.res.Configuration scfg = android.content.res.Resources.getSystem()
                    .getConfiguration();
            b.append(" sys_dm=").append(sdm.widthPixels).append('x').append(sdm.heightPixels)
                    .append('@').append(sdm.densityDpi);
            b.append(" sys_cfg=").append(scfg.screenWidthDp).append('x').append(scfg.screenHeightDp)
                    .append(" smallest=").append(scfg.smallestScreenWidthDp);
        } catch (Throwable x) {
            b.append(" res=<err ").append(unwrap(x)).append('>');
        }
        try {
            Object wm = activity.getClass().getMethod("getSystemService", String.class)
                    .invoke(activity, android.content.Context.WINDOW_SERVICE);
            Object metrics = wm.getClass().getMethod("getCurrentWindowMetrics").invoke(wm);
            android.graphics.Rect bounds = (android.graphics.Rect) metrics.getClass()
                    .getMethod("getBounds").invoke(metrics);
            Object insets = metrics.getClass().getMethod("getWindowInsets").invoke(metrics);
            b.append(" win_metrics=").append(rect(bounds));
            try {
                android.graphics.Insets sb = ((android.view.WindowInsets) insets)
                        .getInsets(android.view.WindowInsets.Type.statusBars());
                android.graphics.Insets nb = ((android.view.WindowInsets) insets)
                        .getInsets(android.view.WindowInsets.Type.navigationBars());
                b.append(" win_insets=status:").append(sb.top)
                        .append(" nav:").append(nb.bottom);
            } catch (Throwable x) {
                b.append(" win_insets=<err ").append(unwrap(x)).append('>');
            }
        } catch (Throwable x) {
            b.append(" win_metrics=<err ").append(unwrap(x)).append('>');
        }
        try {
            Object window = activity.getClass().getMethod("getWindow").invoke(activity);
            View decor = (View) window.getClass().getMethod("getDecorView").invoke(window);
            int[] loc = new int[2];
            decor.getLocationOnScreen(loc);
            b.append(" decor=")
                    .append(decor.getWidth()).append('x').append(decor.getHeight())
                    .append(" measured=").append(decor.getMeasuredWidth()).append('x')
                    .append(decor.getMeasuredHeight())
                    .append(" at=").append(loc[0]).append(',').append(loc[1]);
            Object vr = Class.forName("android.view.View").getMethod("getViewRootImpl")
                    .invoke(decor);
            if (vr == null) {
                b.append(" vr=null");
            } else {
                Object frame = get(vr, "mWinFrame");
                b.append(" win_frame=").append(frame instanceof android.graphics.Rect
                        ? rect((android.graphics.Rect) frame) : String.valueOf(frame));
                b.append(" vr_wh=").append(get(vr, "mWidth")).append('x')
                        .append(get(vr, "mHeight"));
                b.append(" vr_layout_requested=").append(get(vr, "mLayoutRequested"));
            }
        } catch (Throwable x) {
            b.append(" decor=<err ").append(unwrap(x)).append('>');
        }
        return b.toString();
    }

    static String rect(android.graphics.Rect r) {
        return "[" + r.left + ',' + r.top + "][" + r.right + ',' + r.bottom + ']';
    }

    /**
     * ROT3: the guest display geometry for a target rotation (0..3).
     *
     * <p>One quarter turn swaps the panel dimensions; the app window
     * (`APP_W`/`APP_H`) follows, and the `Configuration` is rebuilt with the
     * framework's own fields: `orientation`, `screenWidthDp`/`screenHeightDp`,
     * `smallestScreenWidthDp` (unchanged: the smallest width of a display does
     * not rotate), and the `WindowConfiguration` bounds/appBounds, exactly as
     * `DisplayContent.updateBaseConfiguration` recomputes them.
     */
    static int sRotation = 0;
    static int sBasePanelW = -1;
    static int sBasePanelH = -1;

    static Configuration rotatedConfiguration(int rotation) {
        if (sBasePanelW < 0) {
            sBasePanelW = PANEL_W;
            sBasePanelH = PANEL_H;
        }
        boolean odd = (rotation % 2) != 0;
        PANEL_W = odd ? sBasePanelH : sBasePanelW;
        PANEL_H = odd ? sBasePanelW : sBasePanelH;
        APP_W = PANEL_W;
        APP_H = REPORT_INSETS ? PANEL_H : PANEL_H - STATUS_INSET - NAV_INSET;
        Configuration cfg = displayConfiguration();
        cfg.orientation = PANEL_W > PANEL_H
                ? Configuration.ORIENTATION_LANDSCAPE
                : Configuration.ORIENTATION_PORTRAIT;
        android.graphics.Rect bounds = new android.graphics.Rect(0, 0, PANEL_W, PANEL_H);
        // `Configuration.windowConfiguration` and `WindowConfiguration`'s
        // setters are `@hide`, so they are reached by reflection (same as every
        // other internal field on this path).
        Object wc = get(cfg, "windowConfiguration");
        if (wc != null) {
            android.graphics.Rect app = REPORT_INSETS
                    ? new android.graphics.Rect(0, 0, PANEL_W, PANEL_H)
                    : new android.graphics.Rect(0, STATUS_INSET, PANEL_W, PANEL_H - NAV_INSET);
            try {
                wc.getClass().getMethod("setBounds", android.graphics.Rect.class).invoke(wc, bounds);
                wc.getClass().getMethod("setAppBounds", android.graphics.Rect.class).invoke(wc, app);
                wc.getClass().getMethod("setMaxBounds", android.graphics.Rect.class).invoke(wc, bounds);
            } catch (Throwable t) {
                System.out.println("ACOSLAUNCH MARK rotate-window-configuration-failed " + unwrap(t));
            }
        }
        return cfg;
    }

    /**
     * ROT3: rotate the display to `want` (a target rotation, 0..3) and let the
     * *framework* decide what happens to the Activity.
     *
     * <p>This is the AOSP sequence with the WMS half already done by the host
     * (`IWindowSession.relayout` now answers the rotated frame + rotated
     * `MergedConfiguration`):
     *
     * <ol>
     *   <li>`ActivityThread.handleConfigurationChanged(newConfig, 0)` — the
     *       process-wide configuration (the same entry point the real
     *       `ActivityThread` receives from AMS).</li>
     *   <li>the activity-level decision is `ActivityInfo.getRealConfigChanged()`
     *       against the four configuration bits a rotation changes
     *       (`orientation|screenLayout|screenSize|smallestScreenSize`), i.e.
     *       `ActivityRecord.getRealConfigChanged()`. When the manifest declares
     *       all four, `handleActivityConfigurationChanged` reports the change to
     *       the live instance; otherwise the record is relaunched through
     *       `scheduleRelaunchActivity` + `handleRelaunchActivity`, whose
     *       `handleRelaunchActivityInner` performs the AOSP order (pause →
     *       stop(saveState) → destroy → launch(state restored)).</li>
     *   <li>`ViewRootImpl.mForceNextWindowRelayout` + `requestLayout()` so the
     *       next traversal actually calls `relayoutWindow` and picks up the new
     *       frame (the WMS sends no unsolicited `resized` in ACOS).</li>
     * </ol>
     */
    /**
     * `ActivityRelaunchItem.obtain(...)` for this image's parameter list: the
     * token, the merged configuration, the pending-config-changes int, the
     * window-info instance (when the image has one), and nulls for the pending
     * results/intents lists.
     */
    static Object obtainRelaunchItem(Class<?> cls, IBinder token, Object mergedConfig,
            Object windowInfo) throws Throwable {
        java.lang.reflect.Method best = null;
        for (java.lang.reflect.Method m : cls.getDeclaredMethods()) {
            if (!m.getName().equals("obtain")) continue;
            for (Class<?> p : m.getParameterTypes()) {
                if (p.getName().equals("android.util.MergedConfiguration")) {
                    best = m;
                    break;
                }
            }
            if (best != null) break;
        }
        if (best == null) {
            throw new NoSuchMethodException(cls.getName() + ".obtain(MergedConfiguration)");
        }
        best.setAccessible(true);
        StringBuilder shape = new StringBuilder();
        for (Class<?> p : best.getParameterTypes()) {
            shape.append(' ').append(p.getSimpleName());
        }
        System.out.println("ACOSLAUNCH MARK obtain-shape " + cls.getSimpleName() + shape);
        try {
            return best.invoke(null, relaunchArgs(best, token, mergedConfig, windowInfo));
        } catch (java.lang.reflect.InvocationTargetException e) {
            throw e.getCause() != null ? e.getCause() : e;
        }
    }

    /**
     * ROT4: after the relaunch/config-change transaction has run, force one
     * relayout on the (possibly new) resumed Activity's decor and publish one
     * probe sample at the new size. This is the rotation's bound frame.
     */
    static void finishRotation(int target, boolean relaunch, String rung, int handled) {
        Object activity = null;
        try {
            Object thread = sThread;
            activity = thread == null ? null : resumedActivity(thread);
            if (activity != null) {
                Object window = activity.getClass().getMethod("getWindow").invoke(activity);
                Object decor = window.getClass().getMethod("getDecorView").invoke(window);
                if (decor instanceof View) {
                    Object vr = null;
                    try {
                        // `View.getViewRootImpl()` is `@hide`.
                        vr = decor.getClass().getMethod("getViewRootImpl").invoke(decor);
                    } catch (Throwable ignored) {
                    }
                    if (vr != null) {
                        setFieldForce(vr.getClass(), vr, "mForceNextWindowRelayout", Boolean.TRUE);
                    }
                    ((View) decor).requestLayout();
                }
            }
        } catch (Throwable t) {
            System.out.println("ACOSLAUNCH MARK rotate-relayout-failed " + unwrap(t));
        }
        // One sample at the new size backs the host's reply.
        sDirtyAtMs = 0;
        sTreeDirty.set(true);
        synchronized (sProbeWake) {
            sProbeRequested = true;
            sProbeWake.notifyAll();
        }
        System.out.println("ACOSLAUNCH MARK rotate rotation=" + target
                + " relaunch=" + relaunch + " via=" + rung
                + " handled=0x" + Integer.toHexString(handled)
                + " activity=" + (activity == null ? "<none>" : activity.getClass().getName())
                + " panel=" + PANEL_W + "x" + PANEL_H);
        System.out.flush();
    }

    /** The image's window-info argument instance, or null when it has none. */
    static Object newWindowInfo() {
        for (String name : new String[]{"android.window.ActivityWindowInfo",
                "android.app.ActivityWindowInfo"}) {
            try {
                Class<?> cls = Class.forName(name);
                java.lang.reflect.Constructor<?> ctor = cls.getDeclaredConstructor();
                ctor.setAccessible(true);
                return ctor.newInstance();
            } catch (Throwable ignored) {
            }
        }
        return null;
    }

    /** The first method `name` whose parameters include `pinnedParamType`. */
    static java.lang.reflect.Method findMethod(Class<?> cls, String name, String pinnedParamType) {
        for (java.lang.reflect.Method m : cls.getDeclaredMethods()) {
            if (!m.getName().equals(name)) continue;
            for (Class<?> p : m.getParameterTypes()) {
                if (p.getName().equals(pinnedParamType)) return m;
            }
        }
        return null;
    }

    /**
     * Fill one discovered framework method's arguments by *type*: `Object` is
     * matched by identity against `objects`, `int` takes the pending config
     * changes, `boolean` false (not resumed / no preserveWindow), and any other
     * class is the window-info slot (the given instance, or null).
     */
    static Object[] relaunchArgs(java.lang.reflect.Method m, Object... objects) {
        Class<?>[] params = m.getParameterTypes();
        Object[] args = new Object[params.length];
        for (int i = 0; i < params.length; i++) {
            Class<?> p = params[i];
            if (p == int.class) {
                args[i] = 0;
            } else if (p == boolean.class) {
                args[i] = Boolean.FALSE;
            } else if (p == IBinder.class) {
                args[i] = null;
            } else {
                // Object-typed slots: an exact class match wins, then the
                // window-info instance when the slot is not one we know.
                Object chosen = null;
                for (Object o : objects) {
                    if (o != null && p.isInstance(o)) {
                        chosen = o;
                        break;
                    }
                }
                args[i] = chosen;
            }
        }
        // A `MergedConfiguration` slot always takes the merged configuration
        // even when a broader type (Object) matched a different object first.
        for (int i = 0; i < params.length; i++) {
            if (params[i].getName().equals("android.util.MergedConfiguration")) {
                for (Object o : objects) {
                    if (o != null && params[i].isInstance(o)) args[i] = o;
                }
            }
            if (params[i].getName().endsWith("ActivityWindowInfo")) {
                for (Object o : objects) {
                    if (o != null && params[i].isInstance(o)) args[i] = o;
                }
            }
            if (params[i] == Configuration.class) {
                for (Object o : objects) {
                    if (o instanceof Configuration) args[i] = o;
                }
            }
        }
        // The token slot (first IBinder) is always the live token.
        for (int i = 0; i < params.length; i++) {
            if (params[i] == IBinder.class) args[i] = sCurrentToken;
        }
        // The one int slot: pending config changes for a relaunch, the display
        // id for a configuration-change report (display 0).
        final int intArg = m.getName().equals("handleActivityConfigurationChanged")
                ? 0 : rotationBitsStatic;
        for (int i = 0; i < params.length; i++) {
            if (params[i] == int.class) {
                args[i] = intArg;
            }
        }
        return args;
    }

    /** The four rotation bits, cached for {@link #relaunchArgs}. */
    static int rotationBitsStatic =
            android.content.pm.ActivityInfo.CONFIG_ORIENTATION
                    | android.content.pm.ActivityInfo.CONFIG_SCREEN_LAYOUT
                    | android.content.pm.ActivityInfo.CONFIG_SCREEN_SIZE
                    | android.content.pm.ActivityInfo.CONFIG_SMALLEST_SCREEN_SIZE;

    static void applyRotation(int want) {
        final int target = ((want % 4) + 4) % 4;
        final int rotationBits =
                android.content.pm.ActivityInfo.CONFIG_ORIENTATION
                        | android.content.pm.ActivityInfo.CONFIG_SCREEN_LAYOUT
                        | android.content.pm.ActivityInfo.CONFIG_SCREEN_SIZE
                        | android.content.pm.ActivityInfo.CONFIG_SMALLEST_SCREEN_SIZE;
        try {
            Object thread = sThread;
            if (thread == null) throw new IllegalStateException("no ActivityThread");
            Configuration cfg = rotatedConfiguration(target);
            sRotation = target;
            Class<?> at = Class.forName("android.app.ActivityThread");
            java.lang.reflect.Method hcc =
                    at.getMethod("handleConfigurationChanged", Configuration.class, int.class);
            hcc.invoke(thread, cfg, 0);
            Object r = null;
            Object activities = get(thread, "mActivities");
            if (activities instanceof java.util.Map && sCurrentToken != null) {
                r = ((java.util.Map<?, ?>) activities).get(sCurrentToken);
            }
            if (r == null) {
                throw new IllegalStateException("no ActivityClientRecord for token");
            }
            Object ai = get(r, "activityInfo");
            int handled = 0;
            if (ai != null) {
                // `ActivityInfo.getRealConfigChanged()` is `@hide`.
                try {
                    java.lang.reflect.Method g = ai.getClass().getMethod("getRealConfigChanged");
                    handled = (Integer) g.invoke(ai);
                } catch (Throwable t) {
                    System.out.println("ACOSLAUNCH MARK rotate-getRealConfigChanged-failed "
                            + unwrap(t));
                }
            }
            boolean relaunch = (handled & rotationBits) != rotationBits;
            String rung = relaunch ? "relaunch" : "config-change";
            Class<?> rec = Class.forName("android.app.ActivityThread$ActivityClientRecord");
            // The window-info argument class differs across the 14 tag and
            // this image (`android.window.ActivityWindowInfo` is absent from
            // the boot image), so the relaunch/configuration entry points are
            // *discovered* by name and their arguments filled by type. A null
            // window-info is what AOSP's own local paths pass when there is
            // none to report.
            Object windowInfo = newWindowInfo();
            if (relaunch) {
                // ROT4: the *AMS* side of a relaunch, which ROT3 was missing.
                //
                // `scheduleRelaunchActivity(IBinder)` cannot be used with this
                // Activity: it calls `scheduleRelaunchActivityIfPossible`,
                // whose first guard is
                // `... || r.token instanceof Binder -> return`, and this
                // Activity is attached with a local `Binder` token (the same
                // guard `ActivityGroup` activities hit). So nothing was ever
                // put on `mRelaunchingActivities`, and
                // `handleRelaunchActivity` then aborted at its own guard
                // ("activity not relaunching") and returned:
                // `handleRelaunchActivityInner` (pause -> stop(saveState) ->
                // destroy -> launch(state restored)) never ran. The tree kept
                // the old instance, which is why the count survived a
                // "relaunch" that delivered no lifecycle callbacks.
                //
                // The framework's entry point for the caller that *does* own
                // the record (AMS, and here the local test harness) is
                // `prepareRelaunchActivity`: it creates the relaunch record on
                // `mRelaunchingActivities` and returns it, which is exactly
                // what `handleRelaunchActivity` resolves the token against.
                Class<?> pta =
                        Class.forName("android.app.servertransaction.PendingTransactionActions");
                java.lang.reflect.Constructor<?> ctor = pta.getDeclaredConstructor();
                ctor.setAccessible(true);
                Object actions = ctor.newInstance();
                Class<?> merged = Class.forName("android.util.MergedConfiguration");
                java.lang.reflect.Constructor<?> mergedCtor =
                        merged.getConstructor(Configuration.class, Configuration.class);
                mergedCtor.setAccessible(true);
                // Empty override: `Configuration.updateFrom` treats an empty
                // config as "no delta", the same as a null override.
                Object mergedConfig = mergedCtor.newInstance(cfg, new Configuration());
                // ROT4: deliver the relaunch the way AMS does — one stock
                // `ClientTransaction` whose item is `ActivityRelaunchItem`,
                // executed by the framework's own `TransactionExecutor` on the
                // main thread. Its `preExecute` calls
                // `ActivityThread.prepareRelaunchActivity` (which is what puts
                // the record on `mRelaunchingActivities`); `execute` calls
                // `handleRelaunchActivity`, whose inner path runs pause ->
                // stop(saveState) -> destroy -> launch(state restored).
                //
                // Calling `handleRelaunchActivity` by hand (ROT3) is not enough:
                // nothing was ever prepared on `mRelaunchingActivities` because
                // `scheduleRelaunchActivity(token)` refuses a local `Binder`
                // token (`r.token instanceof Binder` guard in
                // `scheduleRelaunchActivityIfPossible`), so it aborted before
                // any lifecycle callback. Driving the item itself keeps that
                // ordering in the framework, not in this harness.
                Class<?> ctxCls =
                        Class.forName("android.app.servertransaction.ClientTransaction");
                Class<?> iatCls = Class.forName("android.app.IApplicationThread");
                Class<?> relaunchCls =
                        Class.forName("android.app.servertransaction.ActivityRelaunchItem");
                Class<?> resumeCls =
                        Class.forName("android.app.servertransaction.ResumeActivityItem");
                Object tx = ctxCls.getMethod("obtain", iatCls, IBinder.class)
                        .invoke(null, sClient, sCurrentToken);
                Object item = obtainRelaunchItem(relaunchCls, sCurrentToken, mergedConfig,
                        windowInfo);
                Object resumeItem = obtainResume(resumeCls);
                java.util.ArrayList<Object> callbacks = new java.util.ArrayList<Object>();
                callbacks.add(item);
                setPrivateField(ctxCls, tx, "mActivityCallbacks", callbacks);
                setPrivateField(ctxCls, tx, "mLifecycleStateRequest", resumeItem);
                setPrivateFieldIfPresent(ctxCls, tx, "mTransactionItems", callbacks);
                scheduleLocal(tx);
                rung = "ActivityRelaunchItem(ClientTransaction)";
            } else {
                java.lang.reflect.Method hac = findMethod(
                        at, "handleActivityConfigurationChanged",
                        "android.app.ActivityThread$ActivityClientRecord");
                if (hac == null) {
                    throw new IllegalStateException(
                            "ActivityThread.handleActivityConfigurationChanged is absent from "
                                    + "this image");
                }
                hac.setAccessible(true);
                // An empty override is not a zeroed Configuration (framework
                // `Configuration.updateFrom` treats it as "no delta").
                Configuration override = new Configuration();
                override.setTo(new Configuration());
                hac.invoke(thread, relaunchArgs(hac, r, override, windowInfo));
                rung = "handleActivityConfigurationChanged";
            }
            // The transaction is *posted* to the same main looper this method
            // runs on, so the relaunch executes after we return. Forcing the
            // relayout and publishing the probe from a follow-up message means
            // both happen on the **new** window: the previous ordering forced
            // the old Activity's decor, and the relaunched window's first
            // traversal was published as the rotation's frame (a transient
            // layout that only the next input settled).
            final int fTarget = target;
            final boolean fRelaunch = relaunch;
            final String fRung = rung;
            final int fHandled = handled;
            new android.os.Handler(Looper.getMainLooper()).post(new Runnable() {
                @Override
                public void run() {
                    finishRotation(fTarget, fRelaunch, fRung, fHandled);
                }
            });
        } catch (Throwable t) {
            System.out.println("ACOSLAUNCH MARK rotate-failed rotation=" + target + " " + unwrap(t));
            t.printStackTrace(System.out);
            System.out.flush();
        }
    }

    static void startLiveProbe(final Object activity) {
        final android.os.Handler main = new android.os.Handler(Looper.getMainLooper());
        final java.util.concurrent.atomic.AtomicBoolean pending =
                new java.util.concurrent.atomic.AtomicBoolean(false);
        // P10: register the dirty listeners before the first sample and force
        // one immediately (the launch `ready` line waits for a probe sample).
        try {
            Object window = activity.getClass().getMethod("getWindow").invoke(activity);
            Object decor = window.getClass().getMethod("getDecorView").invoke(window);
            if (decor instanceof View) watchProbeRoots((View) decor);
        } catch (Throwable ignored) {
        }
        sDirtyAtMs = 0;
        sTreeDirty.set(true);
        // P3b: the host writes one `PROBE` line to the guest's stdin when a
        // fast tree needs a sample now. The control thread only wakes the
        // probe thread; it never walks or mutates the view tree itself.
        Thread control = new Thread(new Runnable() {
            @Override
            public void run() {
                try {
                    java.io.BufferedReader reader = new java.io.BufferedReader(
                            new java.io.InputStreamReader(System.in));
                    String line;
                    while ((line = reader.readLine()) != null) {
                        String word = line.trim();
                        if (word.equalsIgnoreCase("PROBE")) {
                            synchronized (sProbeWake) {
                                sProbeRequested = true;
                                sProbeWake.notifyAll();
                            }
                            // SETTLE5: a wake that arrives while the previous
                            // sample has not run yet (or long after the last
                            // one) names a starved/blocked main looper instead
                            // of leaving a host-side `probe-floor` timeout
                            // unexplained. Bounded: at most one mark per 500 ms.
                            long nowMs = System.currentTimeMillis();
                            long gap = sProbePostedMs == 0
                                    ? -1 : nowMs - sProbePostedMs;
                            if (sProbePending && gap >= 500
                                    && nowMs - sLastWakeMarkMs >= 500) {
                                sLastWakeMarkMs = nowMs;
                                System.out.println("ACOSLAUNCH MARK probe-stalled"
                                        + " pending_ms=" + gap + " wake=received");
                                System.out.flush();
                            }
                        } else if (word.toUpperCase().startsWith("ROTATE")) {
                            // ROT3: a display rotation. It must run on the main
                            // looper (configuration changes are a main-thread
                            // framework operation), so the control thread only
                            // posts it.
                            final int want;
                            try {
                                want = Integer.parseInt(word.substring(6).trim());
                            } catch (Throwable bad) {
                                System.out.println("ACOSLAUNCH MARK rotate-bad-arg line=" + word);
                                System.out.flush();
                                continue;
                            }
                            main.post(new Runnable() {
                                @Override
                                public void run() {
                                    applyRotation(want);
                                }
                            });
                        }
                    }
                    sProbeControlDead = true;
                    System.out.println("ACOSLAUNCH MARK probe-control-exit reason=eof");
                    System.out.flush();
                } catch (Throwable t) {
                    // No stdin (one-shot launches) is normal; in an interactive
                    // session an ended control thread ends the demand probe, so
                    // it must be named, never swallowed (SETTLE5).
                    sProbeControlDead = true;
                    if (System.getenv("ACOSLAUNCH_INTERACTIVE") != null) {
                        System.out.println("ACOSLAUNCH MARK probe-control-exit reason="
                                + unwrap(t));
                        System.out.flush();
                    }
                }
            }
        }, "acos-probe-control");
        control.setDaemon(true);
        control.start();
        Thread t = new Thread(new Runnable() {
            @Override
            public void run() {
                // One frame is 16.7 ms; a dirty probe waits for a single
                // debounce window after the last dirty mark and is at least
                // P10_PROBE_MIN_GAP_MS after the previous one, so a burst of
                // draw callbacks coalesces into one walk.
                final long debounceMs =
                        envLong("P10_PROBE_DEBOUNCE_MS", 8);
                final long minGapMs = envLong("P10_PROBE_MIN_GAP_MS", 25);
                final long fallbackMs =
                        envLong("P10_PROBE_FALLBACK_MS", 5000);
                // SETTLE5: with no demand channel left, poll at a strict-settle
                // useful cadence instead of the 5 s idle fallback.
                final long deadChannelMs = envLong("P10_PROBE_DEAD_CHANNEL_MS", 300);
                long seq = 0;
                long pendingSince = 0;
                long lastEmitMs = 0;
                while (true) {
                    // Diagnostic: if the main thread has not run a probe for
                    // >2 s it is blocked somewhere; a Java stack from this
                    // thread names the wall (e.g. Surface.nativeLockCanvas).
                    long now = System.currentTimeMillis();
                    if (pending.get()) {
                        if (pendingSince == 0) {
                            pendingSince = now;
                        } else if (now - pendingSince > 2000) {
                            System.err.println("ACOSPROBE-STALL-DETECT pending_ms=" + (now - pendingSince));
                            System.err.flush();
                            dumpMainStack();
                            pendingSince = now;
                        }
                    } else {
                        pendingSince = 0;
                    }
                    if (!pending.get()) {
                        boolean wake;
                        synchronized (sProbeWake) {
                            wake = sProbeRequested;
                            sProbeRequested = false;
                        }
                        final long n;
                        String cause = null;
                        if (wake) {
                            cause = "wake";
                        } else if (sTreeDirty.get()
                                && now - sDirtyAtMs >= debounceMs
                                && now - lastEmitMs >= minGapMs) {
                            cause = "dirty";
                        } else if (now - lastEmitMs >= (sProbeControlDead
                                ? Math.min(fallbackMs, deadChannelMs) : fallbackMs)) {
                            cause = "poll";
                        }
                        if (cause != null) {
                            n = ++seq;
                            sTreeDirty.set(false);
                            lastEmitMs = now;
                            final String why = cause;
                            pending.set(true);
                            sProbePostedMs = now;
                            sProbePending = true;
                            main.post(new Runnable() {
                                @Override
                                public void run() {
                                    try {
                                        // ROT3: after a relaunch the Activity
                                        // instance is a new one; a sample must
                                        // describe the *resumed* Activity, not
                                        // the instance captured at launch.
                                        Object live = resumedActivity(sThread);
                                        // ROT8: remember the newest resumed
                                        // Activity; once it is finished and
                                        // destroyed the framework drops its
                                        // record, and `taskFinished` needs it.
                                        if (live != null) sLastLiveActivity = live;
                                        // ROT8b: prefer any live (non-destroyed)
                                        // record of the process over the stale
                                        // remembered one; the remembered
                                        // Activity is only the fallback once
                                        // nothing is alive (and a new resume
                                        // above already replaced it).
                                        Object sample = live;
                                        if (sample == null) sample = liveRecordActivity();
                                        if (sample == null) {
                                            sample = sLastLiveActivity != null
                                                    ? sLastLiveActivity : activity;
                                        }
                                        emitProbe(sample, n, why);
                                        // H2: the first emitted sample is the
                                        // host's `ready` (first frame). Request
                                        // ART's own background collection now
                                        // instead of waiting for the post-fork
                                        // footprint schedule. On by default;
                                        // ACOS_ART_EARLY_GC=0 turns it off.
                                        if (n == 1) maybeEarlyRelease();
                                    } catch (Throwable t) {
                                        // A probe must never disturb the app, but it
                                        // must not hide its own failure either
                                        // (W36): publish one structured error line
                                        // so a missing window is never a silent
                                        // "probe froze" mystery.
                                        probeError("sample", n, null, t);
                                    } finally {
                                        pending.set(false);
                                        sProbePending = false;
                                    }
                                }
                            });
                        }
                    }
                    // Wait for a view event, a host request, or the fallback
                    // tick, whichever is first. No probe runs per draw: the
                    // listeners only mark dirty (P10).
                    try {
                        long waitMs = (sProbeControlDead
                                ? Math.min(fallbackMs, deadChannelMs) : fallbackMs)
                                - (System.currentTimeMillis() - lastEmitMs);
                        long nowMs = System.currentTimeMillis();
                        long gapWait = minGapMs - (nowMs - lastEmitMs);
                        if (gapWait > waitMs) waitMs = gapWait;
                        if (sTreeDirty.get()) {
                            long dirtyWait = debounceMs - (nowMs - sDirtyAtMs);
                            if (dirtyWait < waitMs) waitMs = dirtyWait;
                        }
                        if (pending.get()) waitMs = 10;
                        if (waitMs < 1) waitMs = 1;
                        synchronized (sProbeWake) {
                            if (!sProbeRequested) {
                                sProbeWake.wait(waitMs);
                            }
                        }
                    } catch (InterruptedException e) {
                        return;
                    }
                }
            }
        }, "acos-live-probe");
        t.setDaemon(true);
        t.start();
    }

    static void dumpMainStack() {
        try {
            StackTraceElement[] frames =
                    Looper.getMainLooper().getThread().getStackTrace();
            StringBuilder b = new StringBuilder("ACOSPROBE-STALL main-thread");
            int n = 0;
            for (StackTraceElement f : frames) {
                if (n++ >= 40) break;
                b.append(" | ").append(f);
            }
            System.out.println(b);
            System.err.println(b);
            System.out.flush();
            System.err.flush();
        } catch (Throwable ignored) {
        }
    }

    /**
     * W36: one structured line for a probe failure, on stdout **and** stderr.
     *
     * <p>W32 proved the dialog window attaches host-side and that the probe
     * throws on the second root; the throw was swallowed by
     * {@code catch (Throwable ignored)}, so the only visible symptom was a
     * frozen {@code seq}. Never swallow silently again: every probe failure is
     * named here (phase, window, first cause, bounded stack) and the launcher
     * host can carry it into a step result.
     */
    static void probeError(String phase, long seq, String window, Throwable t) {
        StringBuilder b = new StringBuilder(512);
        b.append("ACOSPROBE-ERROR {\"seq\":").append(seq)
                .append(",\"phase\":\"").append(jesc(phase)).append('"');
        if (window != null) b.append(",\"window\":\"").append(jesc(window)).append('"');
        b.append(",\"error\":\"").append(jesc(String.valueOf(t))).append('"')
                .append(",\"stack\":[");
        try {
            StackTraceElement[] frames = t.getStackTrace();
            int n = 0;
            for (StackTraceElement f : frames) {
                if (n++ >= 24) break;
                if (n > 1) b.append(',');
                b.append('"').append(jesc(f.toString())).append('"');
            }
        } catch (Throwable ignored) {
        }
        b.append("]}");
        String line = b.toString();
        System.out.println(line);
        System.err.println(line);
        System.out.flush();
        System.err.flush();
    }

    /**
     * H2 (product default): after the first live frame, request one real ART
     * background collection on a daemon thread.
     *
     * <p>{@code ACOS_ART_EARLY_GC=0} turns it off (the A/B off arm and the
     * escape hatch). Otherwise the thread calls the hidden
     * {@code dalvik.system.VMRuntime.getRuntime().requestConcurrentGC()}
     * reflectively: the scheduled CC collection is not app-perceptible, so
     * {@code GarbageCollector::ShouldEagerlyReleaseMemoryToOS()} is true and
     * {@code ClearFromSpace}/{@code ReleaseFreeRegions} returns the region
     * space to the OS within ~1 s (Z7). {@code System.gc()} is not called:
     * Z7 measured it inert on this image (gc-count 3 -> 3, 0 ms).
     * {@code ACOS_ART_GC_DELAY_MS} optionally delays the request (research
     * knob, 0 = at the first frame).
     */
    static void maybeEarlyRelease() {
        final String knob = System.getenv("ACOS_ART_EARLY_GC");
        if (knob != null && knob.trim().equals("0")) return;
        final long delayMs = envLong("ACOS_ART_GC_DELAY_MS", 0L);
        Thread t = new Thread(new Runnable() {
            @Override
            public void run() {
                long t0 = System.nanoTime();
                if (delayMs > 0) {
                    try {
                        Thread.sleep(delayMs);
                    } catch (InterruptedException ignored) {
                    }
                }
                long c0 = runtimeStat("art.gc.gc-count");
                System.out.println("ACOSLAUNCH EARLY_RELEASE start delay_ms=" + delayMs
                        + " gc_count_before=" + c0);
                System.out.flush();
                try {
                    Class<?> vm = Class.forName("dalvik.system.VMRuntime");
                    Object rt = vm.getMethod("getRuntime").invoke(null);
                    vm.getMethod("requestConcurrentGC").invoke(rt);
                } catch (Throwable x) {
                    System.err.println("ACOSLAUNCH EARLY_RELEASE concurrent_error " + x);
                }
                long c1 = runtimeStat("art.gc.gc-count");
                System.out.println("ACOSLAUNCH EARLY_RELEASE done ms="
                        + (System.nanoTime() - t0) / 1000000L
                        + " gc_count_after=" + c1);
                System.out.flush();
            }
        }, "acos-early-release");
        t.setDaemon(true);
        t.start();
    }

    /** One ART runtime statistic (`art.gc.gc-count`), or -1 when unavailable. */
    static long runtimeStat(String name) {
        try {
            String v = android.os.Debug.getRuntimeStat(name);
            return v == null ? -1L : Long.parseLong(v.trim());
        } catch (Throwable x) {
            return -1L;
        }
    }

    static long envLong(String name, long fallback) {
        try {
            String value = System.getenv(name);
            if (value != null && value.length() > 0) return Long.parseLong(value);
        } catch (Throwable ignored) {
        }
        return fallback;
    }

    static void emitProbe(Object activity, long seq, String cause) throws Throwable {
        long probeStart = System.nanoTime();
        Object window = activity.getClass().getMethod("getWindow").invoke(activity);
        Object decor = window.getClass().getMethod("getDecorView").invoke(window);
        if (!(decor instanceof View)) return;
        // W27 item 4: the live tree is a *window* forest, not just the Activity
        // decor. AlertDialog/PopupWindow/Menu roots are separate windows in
        // WindowManagerGlobal; on a device uiautomator sees them. Walk
        // `getWindowViews()` in AOSP list order (oldest/bottom first, newest/top
        // last) and put the Activity decor first so existing selectors see the
        // same main-window nodes before any dialog duplicate. Every node carries
        // a `window` label and screen bounds (`bounds_on_screen`).
        java.util.List<View> roots = windowViews();
        // ROT8: the task is finished when the framework itself says so: this
        // Activity finished and was destroyed, no Activity record of the
        // process is alive, and WindowManagerGlobal holds no live root. A
        // phone would show the launcher; ACOS has none, so the honest state is
        // an empty window forest, reported as settled (never a timeout, never a
        // synthetic frame).
        boolean gone = taskFinished(activity, roots);
        if (!gone && (roots == null || roots.isEmpty())) {
            roots = java.util.Collections.singletonList((View) decor);
        }
        if (gone) roots = java.util.Collections.emptyList();
        StringBuilder nodes = new StringBuilder(2048);
        StringBuilder windows = new StringBuilder(256);
        // W31: the per-window node budget is a hard cap, so the sample must say
        // so. Before W31 a tree that stopped at the cap was reported as if it
        // were the whole hierarchy (`truncated` absent => false), which made a
        // saved tree claim completeness it did not have. One flag for the whole
        // sample: true if any window stopped at the cap.
        boolean[] truncated = new boolean[]{false};
        // LAYER1: window facts (identity, real type/flags, focus, parent) travel
        // with every root. The host applies the AOSP layer policy and tags the
        // nodes; the guest never re-derives z (D3).
        String[] labels = new String[roots.size()];
        for (int i = 0; i < roots.size(); i++) {
            View r = roots.get(i);
            labels[i] = r == decor ? "main" : (r == null ? "" : windowLabel(r, i));
        }
        int z = 0;
        boolean sawMain = false;
        for (View root : roots) {
            if (root == null) continue;
            String label;
            if (root == decor) {
                label = "main";
                sawMain = true;
            } else {
                label = labels[z];
            }
            if (windows.length() > 0) windows.append(',');
            windows.append("{\"z\":").append(z)
                    .append(",\"window\":\"").append(jesc(label)).append("\"")
                    .append(windowMeta(root, roots, labels));
            windows.append('}');
            int[] budget = new int[]{PROBE_NODE_BUDGET};
            // Each root is built in its own buffer: a throw can never leave a
            // half-written object in the shared `nodes` array (W36: the missing
            // comma/partial root made every two-window sample invalid JSON, so
            // the host silently kept the last single-window sample).
            StringBuilder one = new StringBuilder(4096);
            try {
                appendViewJson(root, one, budget, truncated, label, rootOrigin(root));
            } catch (Throwable t) {
                // A probe must never disturb the app; skip one bad window, but
                // say which window failed and why (W36).
                probeError("window", seq, label, t);
            }
            if (one.length() > 0) {
                if (nodes.length() > 0) nodes.append(',');
                nodes.append(one);
            }
            z++;
        }
        if (!sawMain && !gone) {
            // The Activity decor was not in mViews (window not attached yet):
            // keep the old single-window sample rather than an empty tree.
            int[] budget = new int[]{PROBE_NODE_BUDGET};
            StringBuilder one = new StringBuilder(4096);
            try {
                appendViewJson((View) decor, one, budget, truncated, "main",
                        rootOrigin((View) decor));
            } catch (Throwable t) {
                probeError("decor-fallback", seq, "main", t);
                return;
            }
            if (one.length() > 0) {
                if (nodes.length() > 0) nodes.append(',');
                nodes.append(one);
            }
        }
        // P10: a new window root (dialog/menu/popup) is registered for dirty
        // events here, on the main thread and outside any draw dispatch.
        if (!gone) watchProbeRoots((View) decor);
        // P11/P12: strict settle is always the event rule.
        SettleSignals settle;
        if (gone) {
            settle = new SettleSignals();
            settle.horizonMs = settleHorizonMs();
            settle.windowGone = true;
            settle.settled = true;
        } else {
            settle = checkSettle(roots, (View) decor);
        }
        StringBuilder j = new StringBuilder(nodes.length() + 512);
        j.append("ACOSPROBE {\"seq\":").append(seq)
                .append(",\"time\":").append(System.currentTimeMillis())
                .append(",\"cause\":\"").append(cause).append('"')
                .append(",\"view_generation\":").append(sViewGeneration)
                .append(",\"windows\":[").append(windows).append("]")
                .append(",\"tree\":{\"nodes\":[").append(nodes).append(']')
                .append(",\"window_count\":").append(z)
                .append(",\"node_budget\":").append(PROBE_NODE_BUDGET)
                .append(",\"truncated\":").append(truncated[0])
                .append(",\"emit_us\":").append((System.nanoTime() - probeStart) / 1000)
                .append('}');
        if (settle != null) {
            j.append(",\"settle\":{\"settled\":").append(settle.settled)
                    .append(",\"pending\":[");
            for (int i = 0; i < settle.pending.size(); i++) {
                if (i > 0) j.append(',');
                j.append('"').append(jesc(settle.pending.get(i))).append('"');
            }
            j.append("],\"frame_callbacks\":").append(settle.frameCallbacks)
                    .append(",\"queue_due_ms\":").append(settle.queueDueMs)
                    .append(",\"horizon_ms\":").append(settle.horizonMs)
                    .append(",\"anim_callbacks\":[")
                    .append(animCallbacksJson(settle))
                    .append(']')
                    .append(",\"draw_ms\":").append(sLastDrawMs)
                    .append(",\"draw_seq\":").append(sDrawSeq)
                    .append(",\"window_stable_ms\":").append(settle.windowStableMs)
                    .append(",\"in_touch_mode\":").append(settle.inTouchMode)
                    .append(",\"window_gone\":").append(settle.windowGone)
                    .append(",\"task_finished\":").append(settle.windowGone)
                    .append(",\"view_generation\":").append(sViewGeneration)
                    .append('}');
        }
        // P12: the effective animation state travels with every sample, so a
        // reply never claims `animations:off` without the guest confirming it.
        j.append(",\"animations\":{\"on\":").append(sAnimationsOn)
                .append(",\"scale\":").append(sAnimationsScale)
                .append(",\"applied\":").append(sAnimationsApplied)
                .append(",\"source\":\"").append(jesc(sAnimationsSource)).append("\"}");
        j.append('}');
        System.out.println(j);
        System.out.flush();
    }

    /** LAYER1 window facts: identity, real type/flags, focus and parent link. */
    static String windowMeta(View root, java.util.List<View> roots, String[] labels) {
        StringBuilder sb = new StringBuilder(128);
        Object vr = null;
        try {
            vr = Class.forName("android.view.View").getMethod("getViewRootImpl").invoke(root);
        } catch (Throwable ignored) {
        }
        Object own = get(vr, "mWindow");
        if (own != null) {
            sb.append(",\"id\":\"").append(Integer.toHexString(System.identityHashCode(own)))
              .append('"');
        }
        Object attrs = get(vr, "mWindowAttributes");
        int type = -1;
        int flags = 0;
        Object token = null;
        if (attrs != null) {
            Object t = get(attrs, "type");
            Object f = get(attrs, "flags");
            if (t instanceof Integer) type = (Integer) t;
            if (f instanceof Integer) flags = (Integer) f;
            token = get(attrs, "token");
        }
        if (type >= 0) sb.append(",\"type\":").append(type);
        // FLAG_NOT_TOUCHABLE (0x10): the same bit the WMS reads.
        sb.append(",\"touch\":").append((flags & 0x10) == 0);
        // HWFALLBACK: the window asked for FLAG_HARDWARE_ACCELERATED (0x01000000)
        // but the guest has no window GPU renderer (disableWindowRenderer), so it
        // paints in software. The host turns this into a verdict note.
        if ((flags & 0x01000000) != 0) sb.append(",\"hw_fallback\":true");
        boolean focus = false;
        try {
            focus = root.hasWindowFocus();
        } catch (Throwable ignored) {
        }
        sb.append(",\"focus\":").append(focus);
        int[] loc = new int[2];
        try {
            root.getLocationOnScreen(loc);
        } catch (Throwable ignored) {
        }
        sb.append(",\"bounds\":\"[").append(loc[0]).append(',').append(loc[1])
          .append("][").append(loc[0] + root.getWidth()).append(',')
          .append(loc[1] + root.getHeight()).append("]\"");
        if (token != null) {
            for (int i = 0; i < roots.size(); i++) {
                View other = roots.get(i);
                if (other == null || other == root) continue;
                Object otherVr = null;
                try {
                    otherVr = Class.forName("android.view.View").getMethod("getViewRootImpl")
                            .invoke(other);
                } catch (Throwable ignored) {
                }
                if (get(otherVr, "mWindow") == token) {
                    sb.append(",\"parent\":\"").append(jesc(labels[i])).append('"');
                    break;
                }
            }
        }
        return sb.toString();
    }

    /**
     * ROT8: is the app's task finished? True only when (a) `activity` was
     * finished and destroyed by the framework (`mFinished` + `isDestroyed()`),
     * (b) no Activity record in this process holds a live (non-destroyed)
     * Activity, and (c) no window root in WindowManagerGlobal is still attached.
     * All three are the framework's own state, read on the UI thread.
     */
    /** ROT8b: a non-destroyed Activity from `mActivities`, or null. */
    static Object liveRecordActivity() {
        try {
            Object activities = get(sThread, "mActivities");
            if (activities instanceof java.util.Map) {
                for (Object rec : ((java.util.Map<?, ?>) activities).values()) {
                    Object a = get(rec, "activity");
                    if (a == null) continue;
                    if (!Boolean.TRUE.equals(a.getClass().getMethod("isDestroyed").invoke(a))) {
                        return a;
                    }
                }
            }
        } catch (Throwable t) {
            // fall through
        }
        return null;
    }

    static boolean taskFinished(Object activity, java.util.List<View> roots) {
        try {
            if (!Boolean.TRUE.equals(get(activity, "mFinished"))) return false;
            if (!Boolean.TRUE.equals(activity.getClass().getMethod("isDestroyed").invoke(activity))) {
                return false;
            }
            Object activities = get(sThread, "mActivities");
            if (activities instanceof java.util.Map) {
                for (Object rec : ((java.util.Map<?, ?>) activities).values()) {
                    Object a = get(rec, "activity");
                    // ROT8b: a record whose Activity is not (yet) set may be a
                    // launch in flight; it cannot prove the task is finished.
                    if (a == null) return false;
                    if (!Boolean.TRUE.equals(a.getClass().getMethod("isDestroyed").invoke(a))) {
                        return false;
                    }
                }
            }
            if (roots != null) {
                for (View root : roots) {
                    if (root == null) continue;
                    Object vr = viewRootOf(root);
                    if (vr != null && !Boolean.FALSE.equals(get(vr, "mAdded"))) return false;
                }
            }
            return true;
        } catch (Throwable t) {
            return false;
        }
    }

    /** All attached root views, via the stock WindowManagerGlobal API. */
    static java.util.List<View> windowViews() {
        try {
            Class<?> wmg = Class.forName("android.view.WindowManagerGlobal");
            Object instance = wmg.getMethod("getInstance").invoke(null);
            Object views = null;
            Throwable first = null;
            try {
                views = wmg.getMethod("getWindowViews").invoke(instance);
            } catch (Throwable t) {
                first = t;
            }
            if (!(views instanceof java.util.List)) {
                // The image's WindowManagerGlobal keeps the z-ordered roots in
                // the private `mViews` list (mViews/mRoots/mParams are parallel).
                // Reading it is the same read-only reflection used for every
                // other private framework field on this path.
                try {
                    views = get(instance, "mViews");
                } catch (Throwable t) {
                    if (!sProbeWindowsLogged) {
                        sProbeWindowsLogged = true;
                        System.out.println("ACOSLAUNCH MARK window-views-unavailable first="
                                + first + " second=" + t);
                    }
                }
            }
            if (views instanceof java.util.List) {
                @SuppressWarnings("unchecked")
                java.util.List<Object> raw = (java.util.List<Object>) views;
                java.util.List<View> out = new java.util.ArrayList<>();
                for (Object v : raw) if (v instanceof View) out.add((View) v);
                if (!sProbeWindowsLogged) {
                    sProbeWindowsLogged = true;
                    System.out.println("ACOSLAUNCH MARK window-views count=" + out.size());
                }
                if (out.size() != sProbeRootCount) {
                    // W36 diagnostic: the root list is the one thing a missing
                    // dialog depends on; print every change with the stock
                    // class names, never infer the count.
                    sProbeRootCount = out.size();
                    StringBuilder r = new StringBuilder("ACOSPROBE-ROOTS {\"count\":")
                            .append(out.size()).append(",\"views\":[");
                    for (int i = 0; i < out.size(); i++) {
                        if (i > 0) r.append(',');
                        View v = out.get(i);
                        r.append("{\"z\":").append(i)
                                .append(",\"class\":\"").append(jesc(v.getClass().getName()))
                                .append("\",\"window\":\"").append(jesc(windowLabel(v, i)))
                                .append("\"}");
                    }
                    r.append("]}");
                    System.out.println(r);
                    System.out.flush();
                }
                return out;
            }
        } catch (Throwable ignored) {
        }
        return null;
    }

    static volatile boolean sProbeWindowsLogged;
    /** W36 diagnostic: last root count printed by {@link #windowViews()}. */
    static int sProbeRootCount = -1;

    /** A stable window label from ViewRootImpl's own LayoutParams (read-only). */
    static String windowLabel(View root, int z) {
        String title = "";
        Object type = null;
        try {
            java.lang.reflect.Method m = Class.forName("android.view.View")
                    .getMethod("getViewRootImpl");
            Object vr = m.invoke(root);
            if (vr != null) {
                Object attrs = get(vr, "mWindowAttributes");
                if (attrs != null) {
                    Object t = get(attrs, "title");
                    if (t != null) title = t.toString();
                    type = get(attrs, "type");
                }
            }
        } catch (Throwable ignored) {
        }
        if (title == null || title.length() == 0) {
            title = "window-" + (type == null ? "?" : type);
        }
        return title + "#" + z;
    }

    /**
     * How many nodes one window's probe sample may carry (hard cap).
     *
     * <p>W31 measured the old 160-node cap truncating the Gaten release home
     * screen (160/160/160, `truncated:true`). W36 raised it to a measured
     * budget that carries the full Gaten tree and any realistic dialog; the
     * flag stays honest at the new cap. Probe cost is measured per sample in
     * `emit_us` on the probe line.
     */
    static final int PROBE_NODE_BUDGET = 2000;

    /** H15: probe-diagnostic flag, set only by P10_PROBE_ANIM_TRACE=1. */
    static final boolean sAnimTrace = envLong("P10_PROBE_ANIM_TRACE", 0) != 0;

    /** The window root's screen origin (`getGlobalVisibleRect` is root-local). */
    static int[] rootOrigin(View root) {
        int[] origin = new int[2];
        try {
            root.getLocationOnScreen(origin);
        } catch (Throwable ignored) {
        }
        return origin;
    }

    /**
     * The display rect a window/visible rect is clipped against:
     * `[0,0,w,h]` in display pixels of the session display **as it is at this
     * capture**, or all zeros when the display size is unknown (then no display
     * clipping is applied).
     *
     * <p>PARITYFIX F2: this used to be probed once from
     * `v.getResources().getDisplayMetrics()` and cached for the process, so after
     * a rotation every `visible_bounds` was still clipped to the pre-rotation
     * width (landscape 2400x1080 clipped to 1080 wide). The session display is
     * `PANEL_W` x `PANEL_H`, which `rotatedConfiguration()` swaps on every
     * rotation (the same values `DisplayInfo.logicalWidth/Height` and the
     * frame's display rect are built from); it is read here on every call, never
     * cached. `DisplayMetrics` is only the fallback for a panel that was never
     * sized.
     */
    static int[] displayBounds(View v) {
        int w = PANEL_W;
        int h = PANEL_H;
        if (w <= 0 || h <= 0) {
            w = 0;
            h = 0;
            try {
                android.util.DisplayMetrics m = v.getResources().getDisplayMetrics();
                if (m != null && m.widthPixels > 0 && m.heightPixels > 0) {
                    w = m.widthPixels;
                    h = m.heightPixels;
                }
            } catch (Throwable ignored) {
            }
        }
        return new int[]{0, 0, w, h};
    }

    /**
     * The visible rect of `v` in **display** pixels, or null when nothing of it
     * can be seen.
     *
     * <p>This is the framework's own `View.getGlobalVisibleRect(Rect)`: it starts
     * from the view's own rect and asks every parent to clip it
     * (`ViewGroup.getChildVisibleRect` — scroll offsets and pager/scroll-clip),
     * so a warm-mounted page parked at `x=+displayWidth` comes back empty. The
     * rect it returns is in the *root view's* coordinate space, so the root's
     * screen origin is added and the result is clipped to the display: a view
     * outside the window/display is not visible to the user either.
     */
    static int[] visibleRectOnScreen(View v, int[] rootOrigin) {
        try {
            android.graphics.Rect r = new android.graphics.Rect();
            if (!v.getGlobalVisibleRect(r) || r.isEmpty()) return null;
            int left = rootOrigin[0] + r.left;
            int top = rootOrigin[1] + r.top;
            int right = rootOrigin[0] + r.right;
            int bottom = rootOrigin[1] + r.bottom;
            int[] d = displayBounds(v);
            if (d[2] > d[0] && d[3] > d[1]) {
                if (left < d[0]) left = d[0];
                if (top < d[1]) top = d[1];
                if (right > d[2]) right = d[2];
                if (bottom > d[3]) bottom = d[3];
            }
            if (right <= left || bottom <= top) return null;
            return new int[]{left, top, right, bottom};
        } catch (Throwable ignored) {
            return null;
        }
    }

    /** Cached framework `View#isVisibleToUser()` (`public @hide`, API 16+). */
    static java.lang.reflect.Method sVisibleToUser;
    static boolean sVisibleToUserProbed;

    /**
     * The framework's own "can the user see this view?" — `View.isVisibleToUser()`
     * is exactly the check `AccessibilityNodeInfo.setVisibleToUser` /
     * UiAutomator publish: attached (and window visible), the view and every
     * ancestor VISIBLE with alpha &gt; 0, and
     * `getGlobalVisibleRect()` non-empty (not clipped away by an ancestor).
     *
     * <p>The method is `public @hide`; reflection is the same read-only access
     * every other private framework read on this path uses. When it is
     * unavailable the fallback reproduces the same checks through public API
     * (`isShown`, `getAlpha`, `getGlobalVisibleRect`), so the answer never
     * silently degrades to "visible".
     */
    static boolean visibleToUser(View v) {
        if (!sVisibleToUserProbed) {
            sVisibleToUserProbed = true;
            try {
                sVisibleToUser = View.class.getMethod("isVisibleToUser");
                sVisibleToUser.setAccessible(true);
            } catch (Throwable ignored) {
                sVisibleToUser = null;
            }
        }
        if (sVisibleToUser != null) {
            try {
                Object r = sVisibleToUser.invoke(v);
                if (r instanceof Boolean) return ((Boolean) r).booleanValue();
            } catch (Throwable ignored) {
                // One honest fallback below; do not retry the lookup per node.
                sVisibleToUser = null;
            }
        }
        try {
            if (!v.isShown() || v.getAlpha() <= 0f) return false;
            android.graphics.Rect r = new android.graphics.Rect();
            return v.getGlobalVisibleRect(r) && !r.isEmpty();
        } catch (Throwable ignored) {
            return false;
        }
    }

    static void appendViewJson(View v, StringBuilder out, int[] budget, boolean[] truncated,
            String window, int[] rootOrigin) {
        if (budget[0]-- <= 0) {
            truncated[0] = true;
            return;
        }
        int[] loc = new int[2];
        try {
            v.getLocationInWindow(loc);
        } catch (Throwable ignored) {
        }
        int id = View.NO_ID;
        try {
            id = v.getId();
        } catch (Throwable ignored) {
        }
        String idName = "";
        if (isResourceId(id)) {
            try {
                idName = v.getResources().getResourceName(id);
            } catch (Throwable t) {
                try {
                    idName = v.getResources().getResourceEntryName(id);
                } catch (Throwable ignored) {
                }
            }
        }
        // TREE1: the node's semantic fields come from the framework's own
        // AccessibilityNodeInfo for this View -- the object UiAutomator and every
        // accessibility service read. createAccessibilityNodeInfo() runs the
        // View's accessibility delegate (React Native installs
        // ReactAccessibilityDelegate on every node) and TextView's/EditText's
        // own rules, so an EditText reports what the platform reports:
        //   text  = getText() ?: getHint()   (TextView.getTextForAccessibility)
        //   hint  = getHint()                (TextView sets setHintText(mHint))
        //   desc  = getContentDescription()  (the app's accessibilityLabel)
        // A password field's characters never leave the process.
        String text = "";
        String hint = "";
        String contentDescription = "";
        boolean password = false;
        boolean fromNode = false;
        boolean passwordUnknown = false;
        try {
            AccessibilityNodeInfo info = v.createAccessibilityNodeInfo();
            if (info != null) {
                CharSequence cs = info.getText();
                text = cs == null ? "" : cs.toString();
                CharSequence h = info.getHintText();
                hint = h == null ? "" : h.toString();
                CharSequence cd = info.getContentDescription();
                contentDescription = cd == null ? "" : cd.toString();
                // AOSP API 34 TextView.onInitializeAccessibilityNodeInfoInternal sets
                // the text unconditionally -- getTextForAccessibility() -- and only
                // *flags* the node with setPassword(true). So a device (and
                // `uiautomator dump`) exposes a password field's characters and the
                // flag. ACOS mirrors that and publishes the flag; redacting the text
                // here would invent a tree the platform never exposes (and did: it
                // broke gaten-lesson's typed-token assertion).
                password = info.isPassword();
                fromNode = true;
            }
        } catch (Throwable ignored) {
        }
        if (!fromNode) {
            // No node info (a detached/hostile View): fall back to the View's own
            // fields, keeping TextView's accessibility text rule explicit rather
            // than inventing a value the platform would not expose.
            try {
                if (v instanceof TextView) {
                    CharSequence cs = ((TextView) v).getText();
                    text = cs == null ? "" : cs.toString();
                    CharSequence h = ((TextView) v).getHint();
                    hint = h == null ? "" : h.toString();
                    if (text.isEmpty()) text = hint;
                }
            } catch (Throwable ignored) {
            }
            // AOSP TextView.onInitializeAccessibilityNodeInfoInternal flags
            // isAnyPasswordInputType() (TYPE_TEXT_VARIATION_PASSWORD, _WEB_PASSWORD,
            // _VISIBLE_PASSWORD, TYPE_NUMBER_VARIATION_PASSWORD) and
            // hasPasswordTransformationMethod(). Mirror both; if a TextView's
            // state cannot be read, the node is marked password_unknown so a
            // focused one never proves a non-password focus (input stays masked).
            if (v instanceof TextView) {
                try {
                    TextView tv = (TextView) v;
                    int it = tv.getInputType();
                    int variation = it & (android.text.InputType.TYPE_MASK_CLASS
                            | android.text.InputType.TYPE_MASK_VARIATION);
                    password = variation == (android.text.InputType.TYPE_CLASS_TEXT
                                    | android.text.InputType.TYPE_TEXT_VARIATION_PASSWORD)
                            || variation == (android.text.InputType.TYPE_CLASS_TEXT
                                    | android.text.InputType.TYPE_TEXT_VARIATION_WEB_PASSWORD)
                            || variation == (android.text.InputType.TYPE_CLASS_TEXT
                                    | android.text.InputType.TYPE_TEXT_VARIATION_VISIBLE_PASSWORD)
                            || variation == (android.text.InputType.TYPE_CLASS_NUMBER
                                    | android.text.InputType.TYPE_NUMBER_VARIATION_PASSWORD)
                            || tv.getTransformationMethod()
                                    instanceof android.text.method.PasswordTransformationMethod;
                } catch (Throwable t) {
                    passwordUnknown = true;
                    password = true;
                }
            }
            try {
                CharSequence cd = v.getContentDescription();
                contentDescription = cd == null ? "" : cd.toString();
            } catch (Throwable ignored) {
            }
        }
        int[] screen = new int[2];
        try {
            v.getLocationOnScreen(screen);
        } catch (Throwable ignored) {
        }
        String cls;
        try {
            cls = v.getClass().getName();
        } catch (Throwable t) {
            cls = "android.view.View";
        }
        int vis;
        boolean clickable;
        boolean enabled;
        boolean focused;
        boolean checked;
        boolean selected;
        try {
            vis = v.getVisibility();
        } catch (Throwable t) {
            vis = 0;
        }
        try {
            clickable = v.isClickable();
        } catch (Throwable t) {
            clickable = false;
        }
        try {
            enabled = v.isEnabled();
        } catch (Throwable t) {
            enabled = true;
        }
        try {
            focused = v.isFocused();
        } catch (Throwable t) {
            focused = false;
        }
        try {
            checked = v instanceof android.widget.Checkable
                    && ((android.widget.Checkable) v).isChecked();
        } catch (Throwable t) {
            checked = false;
        }
        try {
            selected = v.isSelected();
        } catch (Throwable t) {
            selected = false;
        }
        // User-visible means "on screen", not "VISIBLE flag set": a warm-mounted
        // pager page at x=+displayWidth reports VISIBLE but the user cannot see
        // it. The rect is the framework's clipped global rect in display space.
        int[] visibleRect = visibleRectOnScreen(v, rootOrigin);
        boolean visible = visibleRect != null && visibleToUser(v);
        out.append("{\"class\":\"").append(jesc(cls)).append('"')
                .append(",\"id\":\"").append(jesc(idName)).append('"')
                .append(",\"text\":\"").append(jesc(text)).append('"')
                .append(",\"hint\":\"").append(jesc(hint)).append('"')
                .append(",\"password\":").append(password)
                .append(passwordUnknown ? ",\"password_unknown\":true" : "")
                .append(",\"content_description\":\"").append(jesc(contentDescription)).append('"')
                .append(",\"window\":\"").append(jesc(window)).append('"')
                .append(",\"bounds\":\"[").append(loc[0]).append(',').append(loc[1])
                .append("][").append(loc[0] + v.getWidth()).append(',')
                .append(loc[1] + v.getHeight()).append("]\"")
                .append(",\"bounds_on_screen\":\"[").append(screen[0]).append(',')
                .append(screen[1]).append("][").append(screen[0] + v.getWidth())
                .append(',').append(screen[1] + v.getHeight()).append("]\"")
                .append(",\"visible_bounds\":\"")
                .append(visibleRect == null ? ""
                        : "[" + visibleRect[0] + "," + visibleRect[1] + "]["
                                + visibleRect[2] + "," + visibleRect[3] + "]")
                .append('"')
                .append(",\"visible\":").append(visible)
                .append(",\"visibility\":").append(vis)
                .append(",\"clickable\":").append(clickable)
                .append(",\"enabled\":").append(enabled)
                .append(",\"focused\":").append(focused)
                .append(",\"checked\":").append(checked)
                .append(",\"selected\":").append(selected);
        StringBuilder kids = new StringBuilder();
        boolean first = true;
        if (v instanceof ViewGroup) {
            ViewGroup g = (ViewGroup) v;
            int n = g.getChildCount();
            for (int i = 0; i < n; i++) {
                if (budget[0] <= 0) {
                    // There are children this sample cannot carry: say so.
                    truncated[0] = true;
                    break;
                }
                View child = g.getChildAt(i);
                if (child == null) continue;
                StringBuilder one = new StringBuilder();
                appendViewJson(child, one, budget, truncated, window, rootOrigin);
                if (one.length() == 0) continue;
                if (!first) kids.append(',');
                kids.append(one);
                first = false;
            }
        }
        // WV6: a WebView's page lives behind `ViewDelegate.
        // getAccessibilityNodeProvider()` (the WIRE v1.1 `ax` tree, exposed as
        // virtual descendants). The framework's View walker cannot see those
        // children, so the provider's own standard enumeration is merged here:
        // `findAccessibilityNodeInfosByText("", HOST_VIEW_ID)` returns the page
        // nodes with the engine's display-space bounds. This is the same
        // AccessibilityNodeProvider path an accessibility service uses -- no
        // side channel and no provider-specific type check.
        AccessibilityNodeProvider provider = null;
        try {
            provider = v.getAccessibilityNodeProvider();
        } catch (Throwable ignored) {
        }
        if (provider != null) {
            java.util.List<AccessibilityNodeInfo> virtual = null;
            try {
                virtual = provider.findAccessibilityNodeInfosByText(
                        "", AccessibilityNodeProvider.HOST_VIEW_ID);
            } catch (Throwable t) {
                // Never a silent empty page: the WebView node says so in logcat
                // and the tree simply carries no virtual children this sample.
                android.util.Log.w("ACOSVIEW", "WebView virtual nodes failed: " + t);
            }
            if (virtual == null || virtual.isEmpty()) {
                // GATENWORD: a provider that does not implement the text search
                // (Jetpack Compose's `AndroidComposeView` provider only serves
                // `createAccessibilityNodeInfo` + child ids) is enumerated the way
                // the framework's own accessibility traversal does: the host node's
                // child ids, depth first.
                virtual = enumerateProviderTree(provider);
            }
            if (virtual != null) {
                for (AccessibilityNodeInfo info : virtual) {
                    if (budget[0] <= 0) {
                        truncated[0] = true;
                        break;
                    }
                    StringBuilder one = new StringBuilder();
                    appendVirtualNodeJson(v, info, one, budget, truncated, window);
                    info.recycle();
                    if (one.length() == 0) continue;
                    if (!first) kids.append(',');
                    kids.append(one);
                    first = false;
                }
            }
        }
        if (kids.length() > 0) {
            out.append(",\"children\":[").append(kids).append(']');
        }
        out.append('}');
    }

    /**
     * GATENWORD: every virtual descendant of a View's `AccessibilityNodeProvider`
     * by the standard traversal (host `createAccessibilityNodeInfo(HOST_VIEW_ID)`,
     * then each child id's node), depth first, bounded. This is what the framework's
     * `AccessibilityInteractionController` and UiAutomator walk; Compose exposes its
     * whole semantics tree this way and nothing else.
     */
    static java.util.List<AccessibilityNodeInfo> enumerateProviderTree(
            AccessibilityNodeProvider provider) {
        java.util.ArrayList<AccessibilityNodeInfo> out = new java.util.ArrayList<>();
        try {
            java.lang.reflect.Method childId =
                    AccessibilityNodeInfo.class.getMethod("getChildId", int.class);
            java.lang.reflect.Method childCount =
                    AccessibilityNodeInfo.class.getMethod("getChildCount");
            AccessibilityNodeInfo host =
                    provider.createAccessibilityNodeInfo(AccessibilityNodeProvider.HOST_VIEW_ID);
            if (host == null) return out;
            java.util.ArrayDeque<AccessibilityNodeInfo> stack = new java.util.ArrayDeque<>();
            stack.push(host);
            java.util.HashSet<Integer> seen = new java.util.HashSet<>();
            while (!stack.isEmpty() && out.size() < 4000) {
                AccessibilityNodeInfo cur = stack.pop();
                int n = (Integer) childCount.invoke(cur);
                // push in reverse so the first child pops first (document order)
                java.util.ArrayList<AccessibilityNodeInfo> kids = new java.util.ArrayList<>();
                for (int i = 0; i < n; i++) {
                    long id = (Long) childId.invoke(cur, i);
                    int vid = (int) ((id & 0xffffffff00000000L) >> 32);
                    if (vid == AccessibilityNodeProvider.HOST_VIEW_ID || !seen.add(vid)) continue;
                    AccessibilityNodeInfo k = provider.createAccessibilityNodeInfo(vid);
                    if (k != null) kids.add(k);
                }
                for (int i = kids.size() - 1; i >= 0; i--) stack.push(kids.get(i));
                if (cur != host) out.add(cur);
                // keep emitting in pre-order: children are popped next
            }
        } catch (Throwable t) {
            android.util.Log.w("ACOSVIEW", "provider tree walk failed: " + t);
        }
        return out;
    }

    /**
     * WV6: one page node from a WebView's `AccessibilityNodeProvider`, in the
     * same JSON shape as a View node. Bounds are the provider's screen-space
     * `getBoundsInScreen`; `visible` means the node intersects the WebView's
     * own visible rect, so a selector cannot tap text clipped out of the view.
     */
    static void appendVirtualNodeJson(View host, AccessibilityNodeInfo info,
            StringBuilder out, int[] budget, boolean[] truncated, String window) {
        if (info == null || budget[0]-- <= 0) {
            truncated[0] = true;
            return;
        }
        android.graphics.Rect rect = new android.graphics.Rect();
        try {
            info.getBoundsInScreen(rect);
        } catch (Throwable ignored) {
        }
        int[] origin = new int[2];
        try {
            host.getLocationOnScreen(origin);
        } catch (Throwable ignored) {
        }
        android.graphics.Rect hostRect = new android.graphics.Rect(
                origin[0], origin[1], origin[0] + host.getWidth(), origin[1] + host.getHeight());
        android.graphics.Rect visibleRect = new android.graphics.Rect(rect);
        boolean intersects = visibleRect.intersect(hostRect);
        boolean visible = intersects && !visibleRect.isEmpty()
                && info.isVisibleToUser() && host.isShown();
        String text = "";
        String contentDescription = "";
        try {
            CharSequence cs = info.getText();
            text = cs == null ? "" : cs.toString();
            CharSequence cd = info.getContentDescription();
            contentDescription = cd == null ? "" : cd.toString();
        } catch (Throwable ignored) {
        }
        String cls = "android.view.View";
        try {
            CharSequence cn = info.getClassName();
            if (cn != null && cn.length() > 0) cls = cn.toString();
        } catch (Throwable ignored) {
        }
        boolean clickable = false;
        boolean enabled = true;
        boolean focused = false;
        boolean checked = false;
        boolean selected = false;
        try {
            clickable = info.isClickable();
            enabled = info.isEnabled();
            focused = info.isFocused();
            checked = info.isCheckable() && info.isChecked();
            selected = info.isSelected();
        } catch (Throwable ignored) {
        }
        out.append("{\"class\":\"").append(jesc(cls)).append('"')
                .append(",\"id\":\"\"")
                .append(",\"text\":\"").append(jesc(text)).append('"')
                .append(",\"hint\":\"\"")
                .append(",\"password\":false")
                .append(",\"content_description\":\"").append(jesc(contentDescription)).append('"')
                .append(",\"window\":\"").append(jesc(window)).append('"')
                .append(",\"bounds\":\"[").append(rect.left).append(',').append(rect.top)
                .append("][").append(rect.right).append(',').append(rect.bottom).append("]\"")
                .append(",\"bounds_on_screen\":\"[").append(rect.left).append(',').append(rect.top)
                .append("][").append(rect.right).append(',').append(rect.bottom).append("]\"")
                .append(",\"visible_bounds\":\"")
                .append(visibleRect.isEmpty() ? ""
                        : "[" + visibleRect.left + "," + visibleRect.top + "]["
                                + visibleRect.right + "," + visibleRect.bottom + "]")
                .append('"')
                .append(",\"visible\":").append(visible)
                .append(",\"visibility\":0")
                .append(",\"clickable\":").append(clickable)
                .append(",\"enabled\":").append(enabled)
                .append(",\"focused\":").append(focused)
                .append(",\"checked\":").append(checked)
                .append(",\"selected\":").append(selected)
                .append('}');
    }

    static String jesc(String s) {
        StringBuilder b = new StringBuilder(s.length() + 8);
        for (int i = 0; i < s.length(); i++) {
            char c = s.charAt(i);
            switch (c) {
                case '\\': b.append("\\\\"); break;
                case '"': b.append("\\\""); break;
                case '\n': b.append("\\n"); break;
                case '\r': b.append("\\r"); break;
                case '\t': b.append("\\t"); break;
                default:
                    if (c < 0x20) b.append(' ');
                    else b.append(c);
            }
        }
        return b.toString();
    }

    static volatile boolean sActivityResumed;
    /** SYSUI2: the launch resolved to an Activity that finished inside onCreate. */
    static volatile boolean sFinishDuringCreate;
    /**
     * S3a: why the last completed first traversal did not draw (null = it drew).
     * Recorded in the `POST_RESUME_NOT_READY` failure so a no-frame relaunch
     * names the skipped draw instead of only "timeout".
     */
    static volatile String sTraversalSkipReason;
    /**
     * S3a/S3PERF: what the readiness check actually saw, e.g.
     * {@code traversed=true app_visible=false skip=traversal:view_not_visible}
     * or {@code traversed=true source=onDraw skip=none}. Published in the
     * `activity-resumed.marker` and the `ACOSLAUNCH MARK activity-resumed`
     * line as evidence; it never gates readiness (main parity).
     */
    static volatile String sFirstTraversalEvidence;
    /** The framework printed `<<<<< Finished` for the BIND_APPLICATION message. */
    static volatile boolean sBindFinished;
    /** W38: the app's `IApplicationThread` interface object and its ActivityThread. */
    static Object sClient;
    static Object sThread;
    /** W38: token of the activity on top of the guest stack (a real Binder). */
    static IBinder sCurrentToken;

    /**
     * W27 item 3: replace the cached `IActivityTaskManager` service with a
     * dynamic proxy whose `startActivity*` methods are answered locally.
     *
     * <p>The proxy delegates every other method to the real binder, so the
     * launch path is untouched. `startActivity` returns
     * {@code ActivityManager.START_SUCCESS} after printing one
     * {@code ACOSUNSUPPORTED} line; the host turns that line into a structured
     * {@code UNSUPPORTED_ANDROID_API} step result instead of the guest dying.
     * This is a documented capability gap, not a fake second Activity: no
     * ActivityRecord, lifecycle callback or window is invented.
     */
    static void installStartActivityGuard() {
        try {
            Class<?> atm = Class.forName("android.app.ActivityTaskManager");
            java.lang.reflect.Field singletonField = atm.getDeclaredField("IActivityTaskManagerSingleton");
            singletonField.setAccessible(true);
            Object singleton = singletonField.get(null);
            java.lang.reflect.Field instanceField = findField(singleton.getClass(), "mInstance");
            instanceField.setAccessible(true);
            Object real = instanceField.get(singleton);
            if (real == null) {
                real = atm.getMethod("getService").invoke(null);
            }
            Class<?> iface = Class.forName("android.app.IActivityTaskManager");
            final Object delegate = real;
            Object proxy = java.lang.reflect.Proxy.newProxyInstance(
                    iface.getClassLoader(), new Class<?>[]{iface},
                    new java.lang.reflect.InvocationHandler() {
                        @Override
                        public Object invoke(Object p, java.lang.reflect.Method m, Object[] a) throws Throwable {
                            String n = m.getName();
                            if (n.startsWith("startActivity") || n.startsWith("startActivities")) {
                                String call = "android.app.IActivityTaskManager." + n;
                                android.content.Intent intent = n.startsWith("startActivities")
                                        ? null : findIntent(a);
                                if (intent == null) {
                                    // `startActivities` bundles its Intents in a list:
                                    // there is nothing to resolve one-by-one here, so
                                    // keep the structured marker and fabricate nothing.
                                    System.out.println("ACOSUNSUPPORTED {\"code\":\"UNSUPPORTED_ANDROID_API\","
                                            + "\"call\":\"" + esc(call) + "\","
                                            + "\"detail\":\"ACOS cannot launch a bundled startActivities "
                                            + "request; no Activity was fabricated\"}");
                                    System.out.flush();
                                    return m.getReturnType() == int.class ? 0 : null;
                                }
                                // MPROC1: an implicit intent is decided by the
                                // **host** package manager, exactly as on a
                                // device. That is what lets the capture target
                                // stand in for an installed share sheet / browser
                                // / picker, and it is why this branch no longer
                                // rejects a component-less request here: the old
                                // short-circuit made the host unable to see the
                                // intent at all.
                                // Record it host-side first (W35 decodes the
                                // Intent and `AmsState` records the launch),
                                // then deliver what AMS would deliver.
                                Object result;
                                try {
                                    result = m.invoke(delegate, a);
                                } catch (java.lang.reflect.InvocationTargetException e) {
                                    throw e.getCause() == null ? e : e.getCause();
                                }
                                int code = (result instanceof Integer) ? (Integer) result : 0;
                                if (code != 0) {
                                    // The host refused the launch (START_*): do
                                    // not launch and do not report success.
                                    System.out.println("ACOSLAUNCH MARK start-activity-refused call="
                                            + esc(call) + " code=" + code);
                                    System.out.flush();
                                    return result;
                                }
                                android.content.ComponentName target = intent.getComponent();
                                if (target != null
                                        && !sPkg.equals(target.getPackageName())) {
                                    // MPROC4 step 3b: the component lives in **another
                                    // installed package**, i.e. in another process. The
                                    // host has already recorded the launch (and the need
                                    // for that package's process); this process must not
                                    // instantiate another package's class. The process
                                    // that owns the component launches it (AMS starts it
                                    // and delivers the transaction there).
                                    System.out.println("ACOSLAUNCH MARK start-activity-remote"
                                            + " package=" + esc(target.getPackageName())
                                            + " class=" + esc(target.getClassName()));
                                    System.out.flush();
                                    return result;
                                }
                                if (System.getenv("ACOSLAUNCH_HOST_SCHEDULES") != null) {
                                    // MPROC7 (ADR-0029 amendment): this process's
                                    // launches are host-built. The request is
                                    // already recorded host-side; the peer
                                    // delivers the `LaunchActivityItem` (or the
                                    // `NewIntentItem` for a reused top) to this
                                    // process's own app thread. The guest must
                                    // not instantiate a second activity with a
                                    // guest-minted token: that token is not the
                                    // one the host's `ActivityRecord` carries
                                    // (it would report the wrong token on
                                    // `activityResumed`/`finishActivity`).
                                    System.out.println("ACOSLAUNCH MARK start-activity-host-scheduled"
                                            + " component=" + esc(String.valueOf(target))
                                            + " flags=0x" + Integer.toHexString(intent.getFlags()));
                                    System.out.flush();
                                    return result;
                                }
                                if (target == null) {
                                    // MPROC1: the host accepted the intent without
                                    // an installed target, i.e. the session's
                                    // **capture target** recorded it. The app is
                                    // told exactly what a device's share sheet
                                    // says ("handled"); the reply/control op says
                                    // `captured`. A scripted result arrives later
                                    // as a real ActivityResultItem transaction.
                                    System.out.println("ACOSLAUNCH MARK start-activity-captured"
                                            + " action=" + esc(String.valueOf(intent.getAction()))
                                            + " type=" + esc(String.valueOf(intent.getType()))
                                            + " data=" + esc(String.valueOf(intent.getDataString())));
                                    System.out.flush();
                                    return result;
                                }
                                try {
                                    if (launchLocalActivity(intent)) return result;
                                } catch (Throwable t) {
                                    System.out.println("ACOSUNSUPPORTED {\"code\":\"UNSUPPORTED_ANDROID_API\","
                                            + "\"call\":\"" + esc(call) + "\","
                                            + "\"detail\":\"stock ClientTransaction delivery failed: "
                                            + esc(String.valueOf(unwrap(t))) + "\"}");
                                    System.out.flush();
                                    // START_CANCELED, never a fabricated success.
                                    return m.getReturnType() == int.class ? 2 : null;
                                }
                                System.out.println("ACOSUNSUPPORTED {\"code\":\"UNSUPPORTED_ANDROID_API\","
                                        + "\"call\":\"" + esc(call) + "\","
                                        + "\"detail\":\"the Intent names a component this package cannot "
                                        + "launch; no Activity was fabricated\"}");
                                System.out.flush();
                                return m.getReturnType() == int.class ? 2 : null;
                            }
                            try {
                                return m.invoke(delegate, a);
                            } catch (java.lang.reflect.InvocationTargetException e) {
                                throw e.getCause() == null ? e : e.getCause();
                            }
                        }
                    });
            instanceField.set(singleton, proxy);
            System.out.println("ACOSLAUNCH MARK start-activity-guard installed call=android.app.IActivityTaskManager");
        } catch (Throwable t) {
            System.out.println("ACOSLAUNCH MARK start-activity-guard unavailable " + t);
        }
        System.out.flush();
    }

    /** Walk a class and its superclasses for a field (Singleton.mInstance). */
    static java.lang.reflect.Field findField(Class<?> cls, String name) throws NoSuchFieldException {
        for (Class<?> c = cls; c != null; c = c.getSuperclass()) {
            try {
                return c.getDeclaredField(name);
            } catch (NoSuchFieldException ignored) {
            }
        }
        throw new NoSuchFieldException(name);
    }

    /** The first `android.content.Intent` argument of a startActivity call. */
    static android.content.Intent findIntent(Object[] args) {
        if (args == null) return null;
        for (Object a : args) {
            if (a instanceof android.content.Intent) return (android.content.Intent) a;
        }
        return null;
    }

    /** `Looper.setMessageLogging`: the framework's own message trace. */
    static void installLooperTrace() {
        android.util.Printer printer = new android.util.Printer() {
            @Override
            public void println(String x) {
                if (x.startsWith("<<<<< Finished to ") && x.contains("android.app.ActivityThread$H")) {
                    sBindFinished = true;
                }
                System.out.println("ACOSLAUNCH TRACE " + x);
            }
        };
        Looper.getMainLooper().setMessageLogging(printer);
    }

    /**
     * The AMS side of the launch protocol, driven by real framework state.
     *
     * <p>It waits for {@code ActivityThread.mInitialApplication} (set by the real
     * {@code handleBindApplication} immediately before
     * {@code mInstrumentation.callApplicationOnCreate(app)}), records the
     * `application-created` marker, and then — for `until=activity-resumed` —
     * pushes the one {@code ClientTransaction} that AMS sends to launch the
     * activity, and waits for the real {@code Activity.mResumed} *and* the first
     * traversal of its window (see {@link #awaitFirstTraversal}).
     */
    static void startWatcher(final Object thread, final Class<?> at, final IBinder appThread) {
        Thread t = new Thread(new Runnable() {
            @Override
            public void run() {
                try {
                    watch(thread, at, appThread);
                } catch (Throwable x) {
                    System.out.println("ACOSLAUNCH MARK watcher-error " + x);
                    x.printStackTrace(System.out);
                }
                System.out.flush();
            }
        }, "acos-launch-watcher");
        t.setDaemon(true);
        t.start();
    }

    /**
     * The uid facts a phone reports to an app: kernel uid/gid/groups/caps,
     * the user-namespace map, PMS's own uid answer, and which of the run's
     * mounts are writable. Printed once in the application-created marker.
     */
    static String uidFacts(android.content.Context base) {
        StringBuilder b = new StringBuilder();
        try {
            b.append("\nguest_uid=").append(android.os.Process.myUid());
        } catch (Throwable t) {
            b.append("\nguest_uid=error:").append(t);
        }
        try {
            b.append("\npms_uid=").append(base.getPackageManager().getPackageUid(sPkg, 0));
        } catch (Throwable t) {
            b.append("\npms_uid=error:").append(t);
        }
        try {
            b.append("\nuid_map=").append(readText("/proc/self/uid_map").trim());
        } catch (Throwable t) {
            b.append("\nuid_map=error:").append(t);
        }
        try {
            for (String line : readText("/proc/self/status").split("\n")) {
                for (String key : new String[]{"Uid:", "Gid:", "Groups:", "CapEff:"}) {
                    if (line.startsWith(key)) b.append("\nstatus_").append(line.trim());
                }
            }
        } catch (Throwable t) {
            b.append("\nstatus=error:").append(t);
        }
        try {
            java.io.File files = base.getFilesDir();
            b.append("\nfiles_dir=").append(files).append(" writable=")
                    .append(files != null && files.canWrite());
        } catch (Throwable t) {
            b.append("\nfiles_dir=error:").append(t);
        }
        try {
            String[] names = new java.io.File("/data/app-data").list();
            b.append("\napp_data_entries=").append(java.util.Arrays.toString(names));
        } catch (Throwable t) {
            b.append("\napp_data_entries=error:").append(t);
        }
        return b.toString();
    }

    static String readText(String path) throws java.io.IOException {
        java.io.FileInputStream in = new java.io.FileInputStream(path);
        try {
            byte[] buf = new byte[4096];
            int n = in.read(buf);
            return n <= 0 ? "" : new String(buf, 0, n, "UTF-8");
        } finally {
            in.close();
        }
    }

    /**
     * MPROC8: a process started for a component that is **not an activity**
     * (`bindService`, `startService`, a provider publish).
     *
     * <p>AOSP's `ActiveServices.realStartServiceLocked` runs after
     * `attachApplicationLocked`: the process attaches (`IApplicationThread` is
     * known to the host), `handleBindApplication` completes, and then the host
     * delivers `scheduleCreateService` + `scheduleBindService` onto this
     * process's looper. There is no launch transaction to wait for — waiting for
     * one (the old behaviour) made a bind-only process time out and exit before
     * its caller was ever answered.
     *
     * <p>So: report the dispatch checkpoint, write the marker and return to the
     * looper. The process stays resident (a bound service's process lives as long
     * as the bind, exactly like a device); the launcher ends it at session end or
     * when the activity manager's idle rule names it.
     */
    static void serviceResident() throws Exception {
        awaitBindDispatch(5000);
        write(new File(sOut, "service-resident.marker"),
                "package=" + sPkg
                        + "\nuid=" + android.os.Process.myUid()
                        + "\npid=" + android.os.Process.myPid()
                        + "\nuntil=" + sUntil
                        + "\nbind_application_dispatch_finished=" + sBindFinished + "\n");
        System.out.println("ACOSLAUNCH MARK service-resident package=" + sPkg
                + " uid=" + android.os.Process.myUid()
                + " pid=" + android.os.Process.myPid()
                + " bind-application=" + sBindFinished);
        System.out.flush();
    }

    /**
     * Wait for the framework's own `<<<<< Finished` line for the BIND_APPLICATION
     * message: that is the proof the real `handleBindApplication` (and therefore
     * `Instrumentation.callApplicationOnCreate`) returned normally.
     */
    static boolean awaitBindDispatch(long ms) throws Exception {
        long bindDeadline = System.currentTimeMillis() + ms;
        while (!sBindFinished && System.currentTimeMillis() < bindDeadline) Thread.sleep(5);
        System.out.println("ACOSLAUNCH MARK bind-application-dispatch-finished=" + sBindFinished);
        return sBindFinished;
    }

    static void watch(Object thread, Class<?> at, IBinder appThread) throws Exception {
        long deadline = System.currentTimeMillis() + 20000;
        Object app = get(thread, "mInitialApplication");
        while (app == null && System.currentTimeMillis() < deadline) {
            Thread.sleep(5);
            app = get(thread, "mInitialApplication");
        }
        if (app == null) {
            System.out.println("ACOSLAUNCH MARK watchdog: application not created");
            dumpThreads();
            quit();
            return;
        }
        sApplicationCreated = true;
        Object instr = get(thread, "mInstrumentation");
        String appBase = "?";
        try {
            android.content.Context base = (android.content.Context) get(app, "mBase");
            if (base != null) appBase = base.getPackageName() + "/" + base.getApplicationInfo().sourceDir;
        } catch (Throwable ignored) {
        }
        String marker = "package=" + sPkg + "\napplication=" + app.getClass().getName()
                + "\napplication_classloader_is_apk=true"
                + "\napplication_base=" + appBase
                + "\ninstrumentation=" + (instr == null ? "null" : instr.getClass().getName())
                + "\ninstrumentation_source=framework (bind request instrumentationName=null)"
                // W31: the flags the framework was given for this app, and where
                // they came from (manifest-derived vs the historical default).
                + "\napp_flags=0x" + Integer.toHexString(sAppFlags >= 0 ? sAppFlags : (0x4 | 0x800000))
                + "\napp_flags_source=" + (sAppFlags >= 0 ? sAppFlagsSource : "default")
                + "\nsourceDir=" + sApk
                + uidFacts((android.content.Context) get(app, "mBase")) + "\n";
        write(new File(sOut, "application-created.marker"), marker);
        System.out.println("ACOSLAUNCH MARK application-created package=" + sPkg
                + " application=" + app.getClass().getName()
                + " base=" + appBase
                + " instrumentation=" + (instr == null ? "null" : instr.getClass().getName()));
        System.out.flush();

        // MPROC8: a bind-only / service-only process (see `serviceResident`).
        // Checked before the activity paths: it has no activity to launch and
        // must never run one, and it must not quit while the host's service
        // deliveries (which arrive right after `attach`) are still in flight.
        if ("service-resident".equals(sUntil)) {
            serviceResident();
            return;
        }
        if (!"activity-resumed".equals(sUntil) || sActivity == null) {
            // Wait for the framework's own `<<<<< Finished` line for the
            // BIND_APPLICATION message: that is the proof the real
            // `handleBindApplication` (and therefore
            // `Instrumentation.callApplicationOnCreate`) returned normally.
            awaitBindDispatch(5000);
            awaitFrameProbe();
            quit();
            return;
        }
        // ---- the AMS step: one ClientTransaction to the app thread ---------
        awaitFrameProbe();
        // W38: the activity token is a real `Binder` created here, exactly as
        // `ActivityRecord.token` is a real binder in system_server. It has to be
        // delivered in-process (`IApplicationThread.scheduleTransaction` on the
        // local interface object): a parcel carrying a binder object cannot be
        // marshalled across the peer's byte pipe, which is why the token used to
        // be null (`no activity for token 0x0`). The transaction items are still
        // the framework's own (`LaunchActivityItem` + `ResumeActivityItem`).
        // MPROC7 (ADR-0029 amendment): for a **spawned secondary process** the
        // host builds and delivers the first launch. Its `ClientTransaction`
        // carries a system_server-side activity token, which a guest cannot
        // mint, so the guest waits for the host's
        // `IApplicationThread.scheduleTransaction` instead of marshalling its
        // own. The primary process keeps the local path (its token is the one
        // the guest itself must report back).
        if (System.getenv("ACOSLAUNCH_HOST_SCHEDULES") != null) {
            System.out.println("ACOSLAUNCH MARK waiting-for-host-transaction activity="
                    + sActivity);
            System.out.flush();
            // The milestone is the **host's launch having executed**, not
            // `mResumed`: `ActivityThread.handleResumeActivity` skips
            // `performResumeActivity` for an `mFinished` Activity
            // (`ActivityThread.java:5290` "Skipping resume of ... activity is
            // finishing"), so an Activity that answers and finishes inside
            // `onCreate` (the already-granted permission request,
            // `GrantPermissionsActivity`'s short-circuit) never sets
            // `mResumed`. Waiting for `mResumed` here timed that *legitimate*
            // launch out (`host-transaction-timeout`, SYSUI1 finding) and the
            // `halt(0)` cut the finish/result handshake before the caller got
            // its result. `mCalled` is the framework's own onCreate/onStart
            // completion flag on the record the transaction created.
            Object launched = null;
            long hostDeadline = System.currentTimeMillis() + sResumeMs;
            while (launched == null && System.currentTimeMillis() < hostDeadline) {
                Thread.sleep(5);
                launched = launchedActivity(thread);
            }
            if (launched == null) {
                System.out.println("ACOSLAUNCH MARK host-transaction-timeout activity="
                        + sActivity + " timeout_ms=" + sResumeMs);
                write(new File(sOut, "activity-not-resumed.marker"),
                        "package=" + sPkg + "\nactivity=" + sActivity
                                + "\ntimeout_ms=" + sResumeMs
                                + "\nreason=HOST_TRANSACTION_TIMEOUT\n");
                dumpActivityState(thread, at);
                quit();
                return;
            }
            Object hostToken = get(launched, "mToken");
            if (hostToken instanceof IBinder) {
                sCurrentToken = (IBinder) hostToken;
                // ROT6: the Activity is the top of the task and the last one, so
                // when it finishes (`Activity.finish()`) the **server** owns the
                // rest of the lifecycle: this watcher delivers the three items
                // stock sends (`Pause(finished)`, `Stop`, `Destroy`), and the
                // host turns the app's `activityDestroyed` into the caller's
                // `ActivityResultItem`. It starts here for a resumed Activity
                // and for one that already finished inside `onCreate`: both owe
                // the same destroy before the caller's result can be delivered.
                startTopFinishWatcher(sCurrentToken);
            }
            boolean resumed = Boolean.TRUE.equals(get(launched, "mResumed"));
            boolean finishedDuringCreate = !resumed
                    && Boolean.TRUE.equals(get(launched, "mFinished"));
            if (finishedDuringCreate) {
                // AOSP's own outcome for an Activity that finishes in onCreate:
                // created (and started), never resumed, then paused/stopped/
                // destroyed by the server. Report exactly that, and keep the
                // real main looper alive: the server drives the rest of the
                // lifecycle and the caller's `ActivityResultItem` follows the
                // destroy — a halt here is what lost the result.
                sActivityResumed = true;
                sFinishDuringCreate = true;
                writeResumeMarker(thread, launched,
                        "finish_during_create=true\n"
                                + "resume_skipped=finishing-in-onCreate "
                                + "(ActivityThread.handleResumeActivity)");
                System.out.println("ACOSLAUNCH MARK activity-resumed activity="
                        + launched.getClass().getName() + " window=" + get(launched, "mWindow")
                        + " finish_during_create=true");
                System.out.flush();
                return;
            }
            System.out.println("ACOSLAUNCH MARK host-transaction-resumed activity="
                    + launched.getClass().getName()
                    + " token=" + (sCurrentToken == null ? "none" : "binder:"
                            + Integer.toHexString(System.identityHashCode(sCurrentToken))));
            System.out.flush();
        } else {
            IBinder token = new android.os.Binder();
            try {
                Object tx = buildLaunchTransaction(sClient, token, app, sActivity);
                scheduleLocal(tx);
                sCurrentToken = token;
                // ROT6: the Activity was launched, so it is the top of the task and the
                // last one: when the app finishes it (`Activity.finish()`), the *server*
                // side owns the rest of the lifecycle. Start the watcher that delivers
                // it (see [`startTopFinishWatcher`]).
                startTopFinishWatcher(token);
                System.out.println("ACOSLAUNCH MARK schedule-transaction-delivered activity="
                        + sActivity + " token=binder:"
                        + Integer.toHexString(System.identityHashCode(token))
                        + " transport=local-iapplicationthread");
            } catch (Throwable x) {
                System.out.println("ACOSLAUNCH MARK schedule-transaction-failed " + x);
                x.printStackTrace(System.out);
                dumpActivityState(thread, at);
                quit();
                return;
            }
            System.out.flush();
        }
        long adeadline = System.currentTimeMillis() + sResumeMs;
        while (System.currentTimeMillis() < adeadline) {
            Object activity = resumedActivity(thread);
            if (activity != null) {
                // O3: `mResumed` is set by Instrumentation *before* the
                // fixture's onResume() body runs, so observing it is not
                // readiness. Wait for the real resume message and the first
                // traversal instead of sleeping 200 ms.
                long readyDeadline = Math.min(
                        System.currentTimeMillis() + sResumeReadyMs, adeadline);
                if (!awaitFirstTraversal(activity, readyDeadline)) {
                    System.out.println("ACOSLAUNCH MARK activity-not-ready"
                            + " reason=first-traversal-timeout deadline_ms=" + sResumeReadyMs
                            + " draw_skip=" + (sTraversalSkipReason == null
                                    ? "none" : sTraversalSkipReason));
                    System.out.println("ACOSLAUNCH {\"outcome\":\"crash\","
                            + "\"code\":\"POST_RESUME_NOT_READY\",\"activity_resumed\":true,"
                            + "\"reason\":\"first-traversal-timeout\",\"deadline_ms\":"
                            + sResumeReadyMs + ",\"draw_skip\":\""
                            + esc(sTraversalSkipReason == null ? "none" : sTraversalSkipReason)
                            + "\"}");
                    System.out.flush();
                    write(new File(sOut, "activity-not-resumed.marker"),
                            "package=" + sPkg + "\nactivity=" + sActivity
                                    + "\nreason=POST_RESUME_READY_TIMEOUT"
                                    + "\ntimeout_ms=" + sResumeReadyMs
                                    + "\nresume_timeout_ms=" + sResumeMs + "\n");
                    dumpActivityState(thread, at);
                    quit();
                    return;
                }
                sActivityResumed = true;
                writeResumeMarker(thread, activity, "resume_ready=first-traversal");
                // R3 test seam: crash *after* the window is up (the shape of the
                // Gaten 1/10 SIGSEGV), so the crash-evidence path is exercised by
                // a deliberate fault instead of waiting for a race. Diagnostic
                // only: the launcher forwards the variable, nothing else reads it.
                crashTestIfRequested(thread, activity);
                System.out.println("ACOSLAUNCH MARK activity-resumed activity="
                        + activity.getClass().getName() + " window=" + get(activity, "mWindow")
                        + " resume_ready=first-traversal"
                        + " draw_skip=" + (sTraversalSkipReason == null
                                ? "none" : sTraversalSkipReason)
                        + " first_traversal=\"" + sFirstTraversalEvidence + "\"");
                // W10 interactive session: instead of a one-shot hold + quit,
                // keep the real main looper alive and publish a read-only view
                // tree sample every ~50 ms. The host's line-JSON session reads
                // these samples as the app's live UI; nothing is mutated.
                // Off unless the launcher forwards the variable.
                if (System.getenv("ACOSLAUNCH_LIVE_PROBE") != null) {
                    if (System.getenv("ACOSLAUNCH_GEOM_TRACE") != null) startGeometryTrace(activity);
                    startLiveProbe(activity);
                    System.out.println("ACOSLAUNCH MARK live-probe-started");
                    System.out.flush();
                    return;
                }
                // Diagnostic hold (W5): the watcher's `quit()` stops the main
                // looper before the first traversal's draw, so a run that ends
                // at `activity-resumed` never reaches `lockCanvas`/`queueBuffer`.
                // Holding here keeps the real looper alive, which is the only
                // way to observe the frame path without inventing a second
                // stop point. Off unless the launcher forwards the variable.
                String hold = System.getenv("ACOSLAUNCH_HOLD_MS");
                if (hold != null) {
                    try {
                        long ms = Long.parseLong(hold);
                        // I2: probe once the first traversal has laid the tree
                        // out, before the hold elapses, so the diagnostic touch
                        // listener is installed before a host-injected tap.
                        Thread.sleep(Math.min(400L, ms / 2));
                        probeInputTree(activity);
                        Thread.sleep(Math.max(0L, ms - Math.min(400L, ms / 2)));
                        System.out.println("ACOSLAUNCH MARK held-after-resume ms=" + ms);
                        probeViewRoot(activity);
                        // I2: read-only live view tree probe. The tap
                        // coordinates come from the *live* post-layout bounds
                        // here, not from the offline renderer. The same probe
                        // after a host-injected tap shows the counter text.
                        probeInputTree(activity);
                    } catch (Throwable ignored) {
                    }
                }
                quit();
                return;
            }
            Thread.sleep(5);
        }
        System.out.println("ACOSLAUNCH MARK watchdog: activity not resumed timeout_ms=" + sResumeMs);
        dumpActivityState(thread, at);
        write(new File(sOut, "activity-not-resumed.marker"),
                "package=" + sPkg + "\nactivity=" + sActivity + "\ntimeout_ms=" + sResumeMs + "\n");
        quit();
    }

    /**
     * O3: wait for the *completed* resume, not just `mResumed`.
     *
     * <p>{@code Instrumentation.callActivityOnResume} sets {@code mResumed}
     * before it calls the fixture's {@code onResume()}
     * ({@code base/core/java/android/app/Instrumentation.java:1721}), so a
     * watcher that publishes readiness on {@code mResumed} races the app's own
     * resume work — that race is what the removed 200 ms sleep papered over.
     * A post to the main looper runs behind the whole resume message: the
     * {@code LAUNCH_ACTIVITY} transaction on the main thread, in
     * {@code ActivityThread.handleResumeActivity}
     * ({@code base/core/java/android/app/ActivityThread.java:5268}), which is
     * where {@code performResumeActivity} (onResume), {@code addView} and
     * {@code makeVisible} complete. The posted check reports ready as soon as
     * a traversal exists, exactly as main does (S3PERF: waiting for the draw
     * here costs one frame on every launch). When the traversal has not run
     * yet, the check installs an {@code OnDrawListener} and
     * {@code ViewRootImpl.performDraw} dispatches it
     * ({@code base/core/java/android/view/ViewRootImpl.java:5227}) during the
     * first {@code performTraversals} ({@code ...:3159}), which the
     * {@code Choreographer} runs as its own main-thread callback.
     *
     * <p>A first traversal that skipped its draw is recorded as evidence
     * ({@code draw_skip=}, {@code first_traversal=} in the marker and the
     * {@code ACOSLAUNCH MARK activity-resumed} line), not treated as a failure:
     * it is the normal pre-visibility traversal, and the host's oneway
     * {@code dispatchAppVisibility(true)} makes the next frame draw. The
     * launcher's no-frame loop bounds the genuinely-stuck case.
     *
     * <p>No timer stands in for the condition: the only time value is the hard
     * deadline. If it expires the caller publishes a structured
     * {@code POST_RESUME_NOT_READY} failure rather than reporting "ready".
     *
     * @return true when the first traversal completed, false on the deadline.
     */
    static boolean awaitFirstTraversal(final Object activity, long deadlineMs) {
        final java.util.concurrent.CountDownLatch latch =
                new java.util.concurrent.CountDownLatch(1);
        final java.util.concurrent.atomic.AtomicBoolean ready =
                new java.util.concurrent.atomic.AtomicBoolean(false);
        try {
            android.os.Handler main = new android.os.Handler(Looper.getMainLooper());
            main.post(new Runnable() {
                @Override
                public void run() {
                    try {
                        Object window = activity.getClass().getMethod("getWindow").invoke(activity);
                        if (window == null) return;
                        Object decorObj = window.getClass().getMethod("getDecorView").invoke(window);
                        if (!(decorObj instanceof View)) return;
                        final View decor = (View) decorObj;
                        // A window path that already traversed synchronously
                        // (not the normal Choreographer path) has no pending
                        // draw to observe; report the state we actually see.
                        // This is main's readiness rule unchanged (S3PERF):
                        // waiting for the draw here costs one frame on *every*
                        // launch, because the normal first traversal runs
                        // before the host's oneway `dispatchAppVisibility(true)`
                        // and therefore skips its draw (AOSP sets `mFirst=false`
                        // before the draw decision, ViewRootImpl:4015 vs
                        // performDraw:4093). So a skipped traversal is
                        // *evidence*, never a gate: its reason and the state
                        // the check saw go into the marker / `ACOSLAUNCH` line,
                        // and the launcher's no-frame loop turns "skipped and
                        // still no C1 frame" into `POST_RESUME_NOT_READY`.
                        if (firstTraversalDone(decor)) {
                            String skip = drawSkipReason(decor);
                            sTraversalSkipReason = skip;
                            sFirstTraversalEvidence = firstTraversalEvidence(decor, skip);
                            System.out.println("ACOSLAUNCH MARK "
                                    + (skip == null ? "first-traversal-drew"
                                                    : "first-traversal-skipped " + skip)
                                    + " " + sFirstTraversalEvidence);
                            System.out.flush();
                            ready.set(true);
                            latch.countDown();
                            return;
                        }
                        final ViewTreeObserver observer = decor.getViewTreeObserver();
                        final ViewTreeObserver.OnDrawListener[] self =
                                new ViewTreeObserver.OnDrawListener[1];
                        self[0] = new ViewTreeObserver.OnDrawListener() {
                            @Override
                            public void onDraw() {
                                // An OnDrawListener cannot be removed from
                                // inside onDraw (ViewTreeObserver.java:794).
                                // The post also puts the signal behind the
                                // rest of performTraversals.
                                new android.os.Handler(Looper.getMainLooper()).post(
                                        new Runnable() {
                                            @Override
                                            public void run() {
                                                try {
                                                    observer.removeOnDrawListener(self[0]);
                                                } catch (Throwable ignored) {
                                                }
                                                // The first traversal ran after
                                                // this check was posted and drew:
                                                // record the evidence the marker
                                                // publishes (no gate, no wait
                                                // beyond main's own rule).
                                                sTraversalSkipReason = null;
                                                sFirstTraversalEvidence =
                                                        "traversed=true source=onDraw skip=none";
                                                ready.set(true);
                                                latch.countDown();
                                            }
                                        });
                            }
                        };
                        observer.addOnDrawListener(self[0]);
                    } catch (Throwable x) {
                        System.out.println("ACOSLAUNCH MARK first-traversal-error " + x);
                    }
                }
            });
            long remaining = deadlineMs - System.currentTimeMillis();
            if (remaining <= 0) return false;
            return latch.await(remaining, java.util.concurrent.TimeUnit.MILLISECONDS)
                    && ready.get();
        } catch (Throwable x) {
            System.out.println("ACOSLAUNCH MARK first-traversal-error " + x);
            return false;
        }
    }

    /**
     * S3a: why the last completed traversal did not draw, or {@code null} when
     * it did. Reads only framework state that `performTraversals` itself
     * maintains (`mLastPerformTraversalsSkipDrawReason`,
     * `mLastPerformDrawSkippedReason`, `mLastDrawScreenOff`,
     * `mLastTraversalWasVisible`); no timer and no fabricated result.
     */
    static String drawSkipReason(View decor) {
        try {
            Object root = Class.forName("android.view.View")
                    .getMethod("getViewRootImpl").invoke(decor);
            if (root == null) return "no_view_root";
            Object traversalSkip = get(root, "mLastPerformTraversalsSkipDrawReason");
            if (traversalSkip != null) return "traversal:" + traversalSkip;
            Object drawSkip = get(root, "mLastPerformDrawSkippedReason");
            if (drawSkip != null) return "draw:" + drawSkip;
            if (Boolean.TRUE.equals(get(root, "mLastDrawScreenOff"))) return "draw:screen_off";
            if (Boolean.FALSE.equals(get(root, "mLastTraversalWasVisible"))) {
                return "traversal:not_visible";
            }
            return null;
        } catch (Throwable x) {
            return "probe_error:" + x;
        }
    }

    /**
     * S3a evidence: the framework state the readiness check saw. Read-only
     * reflection over `ViewRootImpl`; never changes the traversal or waits.
     */
    static String firstTraversalEvidence(View decor, String skip) {
        String appVisible = "?";
        try {
            Object root = Class.forName("android.view.View")
                    .getMethod("getViewRootImpl").invoke(decor);
            if (root != null) {
                Object v = get(root, "mAppVisible");
                if (v != null) appVisible = String.valueOf(v);
            }
        } catch (Throwable ignored) {
        }
        return "traversed=true app_visible=" + appVisible
                + " skip=" + (skip == null ? "none" : skip);
    }

    /** Whether the decor's `ViewRootImpl` has already completed a traversal. */
    static boolean firstTraversalDone(View decor) {
        try {
            Object root = Class.forName("android.view.View")
                    .getMethod("getViewRootImpl").invoke(decor);
            if (root == null) return false;
            return Boolean.FALSE.equals(get(root, "mFirst"));
        } catch (Throwable ignored) {
            return false;
        }
    }

    /**
     * Write the `activity-resumed` marker from real framework state.
     *
     * <p>Two callers, one source of truth: the watcher (when it observes
     * {@code Activity.mResumed}) and the looper-failure path (when the throw
     * happened *after* {@code performResumeActivity}, i.e. in
     * {@code WindowManagerGlobal.addView}). Both read the same
     * {@code Activity}/{@code ActivityClientRecord} fields, so the marker can
     * never claim more than the framework's own state — and `note` is where the
     * reason for the second case is recorded.
     */
    /**
     * ROT7: which half of the framework's back handling is armed for `activity`.
     *
     * <p>API 34 `Activity.onCreate` registers `mDefaultBackCallback`
     * (`this::onBackInvoked`) on the window's `OnBackInvokedDispatcher` when
     * `WindowOnBackInvokedDispatcher.isOnBackInvokedCallbackEnabled(activity)`
     * (targetSdk >= T). When it is registered,
     * `ViewRootImpl$NativePreImeInputStage.doOnBackKeyEvent` claims
     * `KEYCODE_BACK_UP` and calls `onBackInvoked()`; `Activity.onKeyUp`'s legacy
     * branch (`... && mDefaultBackCallback == null`) is then unreachable. When
     * it is not, `ViewRootImpl` registers a `CompatOnBackInvokedCallback` and
     * the legacy path runs. Both end in
     * `ActivityClient.onBackPressed(token, callback)`, so the reported facts are
     * what distinguish "who claimed the key" from "the server refused the
     * finish".
     */
    static String backDispatcherFacts(Object activity) {
        StringBuilder b = new StringBuilder();
        try {
            Class<?> wobi = Class.forName("android.window.WindowOnBackInvokedDispatcher");
            Object enabled = null;
            for (java.lang.reflect.Method m : wobi.getDeclaredMethods()) {
                if (!m.getName().equals("isOnBackInvokedCallbackEnabled")) continue;
                if (m.getParameterCount() != 1) continue;
                m.setAccessible(true);
                enabled = m.invoke(null, activity);
                break;
            }
            b.append("ahead_of_time=").append(enabled);
        } catch (Throwable t) {
            b.append("ahead_of_time=error:").append(unwrap(t));
        }
        Object callback = null;
        try {
            callback = get(activity, "mDefaultBackCallback");
        } catch (Throwable ignored) {
        }
        b.append(" mDefaultBackCallback=").append(callback == null ? "null" : "set");
        try {
            Object dispatcher = activity.getClass().getMethod("getOnBackInvokedDispatcher")
                    .invoke(activity);
            Object top = null;
            for (java.lang.reflect.Method m : dispatcher.getClass().getDeclaredMethods()) {
                if (!m.getName().equals("getTopCallback")) continue;
                m.setAccessible(true);
                top = m.invoke(dispatcher);
                break;
            }
            b.append(" top_callback=").append(top == null ? "null" : top.getClass().getName());
        } catch (Throwable t) {
            b.append(" top_callback=error:").append(unwrap(t));
        }
        return b.toString();
    }

    static String writeResumeMarker(Object thread, Object activity, String note) throws Exception {
        String am = "package=" + sPkg + "\nactivity=" + activity.getClass().getName()
                + "\nlifecycle=onCreate,onStart,onResume"
                + "\nresumed=" + get(activity, "mResumed")
                + "\ncalled=" + get(activity, "mCalled")
                + "\nstopped=" + get(activity, "mStopped")
                + "\nwindow=" + get(activity, "mWindow")
                + "\ntimeout_ms=" + sResumeMs
                // S3PERF: the skipped-draw evidence is published here (and in
                // the `activity-resumed` MARK line). It is diagnostic only: the
                // ready path is main's, and the launcher's no-frame loop reads
                // `draw_skip=` to bound a skip that never draws.
                + "\nfirst_traversal=" + (sFirstTraversalEvidence == null
                        ? "unknown" : sFirstTraversalEvidence)
                + "\ndraw_skip=" + (sTraversalSkipReason == null
                        ? "none" : sTraversalSkipReason)
                // ROT7: who owns a KEYCODE_BACK_UP for this Activity. API 34 has
                // two halves — the ahead-of-time `OnBackInvokedDispatcher` and
                // the legacy `Activity.onKeyUp` branch — and only one of them
                // can claim the key. Recorded, not forced.
                + "\nback_dispatcher=" + backDispatcherFacts(activity)
                + (note == null ? "" : "\n" + note) + "\n";
        write(new File(sOut, "activity-resumed.marker"), am);
        return am;
    }

    /**
     * Record the resume the framework reached before the main looper threw.
     *
     * <p>`handleResumeActivity` runs `performResumeActivity` (which calls
     * `Activity.onResume` and sets `mResumed`) before
     * `WindowManagerGlobal.addView`; a throw in the window path therefore leaves
     * a genuinely resumed activity behind. Returns whether one was found.
     */
    static boolean recordResumeAfterLooperFailure(Object thread, Class<?> at, Throwable fatal)
            throws Exception {
        Object activity = resumedActivity(thread);
        if (activity == null) return false;
        writeResumeMarker(thread, activity,
                "post_resume_failure=" + unwrap(fatal).getClass().getName() + ": " + unwrap(fatal).getMessage());
        sActivityResumed = true;
        System.out.println("ACOSLAUNCH MARK activity-resumed activity=" + activity.getClass().getName()
                + " window=" + get(activity, "mWindow") + " recorded_after=looper-threw");
        System.out.flush();
        return true;
    }

    /**
     * SYSUI2: the Activity the host's launch transaction created.
     *
     * <p>`mCalled` is what `Instrumentation.callActivityOnCreate`/
     * `callActivityOnStart` set on the Activity itself, so a record whose
     * Activity has it is a record whose `onCreate`/`onStart` ran — the proof
     * the host's `LaunchActivityItem` executed. It is deliberately *not*
     * `mResumed`: an Activity that finishes inside `onCreate` never reaches
     * resume (see the call site).
     */
    static Object launchedActivity(Object thread) {
        Object activities = get(thread, "mActivities");
        if (!(activities instanceof java.util.Map)) return null;
        for (Object rec : ((java.util.Map<?, ?>) activities).values()) {
            Object a = get(rec, "activity");
            if (a == null) continue;
            if (Boolean.TRUE.equals(get(a, "mCalled"))) return a;
        }
        return null;
    }

    static Object resumedActivity(Object thread) {
        Object activities = get(thread, "mActivities");
        if (!(activities instanceof java.util.Map)) return null;
        for (Object rec : ((java.util.Map<?, ?>) activities).values()) {
            Object a = get(rec, "activity");
            if (a == null) continue;
            if (Boolean.TRUE.equals(get(a, "mResumed"))) return a;
        }
        return null;
    }

    static void dumpActivityState(Object thread, Class<?> at) {
        for (String f : new String[]{"mBoundApplication", "mInitialApplication", "mInstrumentation",
                "mSystemThread"}) {
            try {
                System.out.println("ACOSLAUNCH MARK " + f + "=" + get(thread, f));
            } catch (Throwable x) {
                System.out.println("ACOSLAUNCH MARK " + f + " -> " + x);
            }
        }
        Object activities = get(thread, "mActivities");
        if (activities instanceof java.util.Map) {
            java.util.Map<?, ?> m = (java.util.Map<?, ?>) activities;
            System.out.println("ACOSLAUNCH MARK mActivities=" + m.size());
            for (java.util.Map.Entry<?, ?> e : m.entrySet()) {
                Object rec = e.getValue();
                System.out.println("ACOSLAUNCH MARK   token=" + e.getKey()
                        + " info=" + get(rec, "activityInfo")
                        + " activity=" + get(rec, "activity")
                        + " paused=" + get(rec, "paused")
                        + " stopped=" + get(rec, "stopped"));
                Object a = get(rec, "activity");
                if (a != null) {
                    System.out.println("ACOSLAUNCH MARK   resumed=" + get(a, "mResumed")
                            + " called=" + get(a, "mCalled")
                            + " finished=" + get(a, "mFinished")
                            + " window=" + get(a, "mWindow"));
                }
            }
        }
        dumpThreads();
    }

    static void dumpThreads() {
        for (java.util.Map.Entry<Thread, StackTraceElement[]> e
                : Thread.getAllStackTraces().entrySet()) {
            System.out.println("ACOSLAUNCH THREAD " + e.getKey().getName() + " "
                    + e.getKey().getState());
            int n = 0;
            for (StackTraceElement el : e.getValue()) {
                if (n++ >= 12) break;
                System.out.println("ACOSLAUNCH   at " + el);
            }
        }
        System.out.flush();
    }

    /**
     * End the run. `prepareMainLooper()`'s queue is quit-disabled
     * (`MessageQueue.quit: "Main thread not allowed to quit"`), which is exactly
     * the "the process does not own its own lifetime" rule a real app lives
     * under; the harness is the thing that owns it, so it ends the process the
     * way AMS would (the markers are already on disk and flushed).
     */
    static void quit() {
        System.out.flush();
        Runtime.getRuntime().halt(0);
    }

    /** `Object` field read through the class hierarchy (framework fields). */
    static Object get(Object o, String name) {
        if (o == null) return null;
        for (Class<?> c = o.getClass(); c != null; c = c.getSuperclass()) {
            try {
                Field f = c.getDeclaredField(name);
                f.setAccessible(true);
                return f.get(o);
            } catch (NoSuchFieldException ignored) {
            } catch (Throwable t) {
                return "<" + t + ">";
            }
        }
        return null;
    }

    /** Construct libandroid_runtime's AndroidRuntime (driver JNI helper). */
    static native int initAndroidRuntime(String argv0);

    // ------------------------------------------------------------- natives

    /** True when the launcher selected a guest GLES provider (`acos.guest.gles` set). */
    static boolean guestGlesSelected() {
        try {
            Class<?> c = Class.forName("android.os.SystemProperties");
            Object v = c.getMethod("get", String.class, String.class).invoke(null, "acos.guest.gles", "");
            return v != null && !String.valueOf(v).isEmpty();
        } catch (Throwable t) {
            return false;
        }
    }

    static void registerNatives() {
        RenderNat.load();
        String[] names = {
                // ActivityThread's own natives (nInitZygoteChildHeapProfiling is
                // called from handleBindApplication).
                "register_android_app_ActivityThread",
                // W31: `Activity.performStart` reads
                // `getDlWarning()` only when the app's
                // `ApplicationInfo.flags` has FLAG_DEBUGGABLE. The guest used to
                // hardcode flags without it; once the manifest-derived flags are
                // truthful (Gaten debug is debuggable) the framework calls this
                // native, so the image's own registration must be present.
                // Symbol: `_ZN7android29register_android_app_ActivityEP7_JNIEnv`
                // (`nm -D .cache/art/sysroot/system/lib64/libandroid_runtime.so`).
                "register_android_app_Activity",
                // LoadedApk.createOrUpdateClassLoaderLocked -> libnativeloader
                // (the APK's class loader namespace).
                "register_com_android_internal_os_ClassLoaderFactory",
                "register_android_util_Log",
                "register_android_util_EventLog",
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
                // The real ViewRootImpl path (P4c → W1/V1) reaches
                // `SurfaceControl.<clinit>` (ViewRootImpl.java:670
                // `SurfaceControl$Transaction`) and then `SurfaceControl`
                // itself, so the class's natives must be registered. E4H §3.1
                // excluded this registration because registration itself ran
                // the class initialiser while the native was still missing;
                // the new launch DOES need the class, so the ordering issue is
                // now observable instead of avoided (L1.md).
                "register_android_view_SurfaceControl",
                // `ViewRootImpl.relayoutWindow` turns the `outSurfaceControl` from
                // `IWindowSession.relayout` into a Java `Surface` with
                // `Surface.copyFrom` -> `nativeGetFromSurfaceControl`. The
                // `Surface` natives were never registered either, so registering
                // `SurfaceControl` alone is not enough (W4).
                "register_android_view_Surface",
                "register_android_graphics_BLASTBufferQueue",
                // `ViewRootImpl.<init>` (line 724 on API 34) constructs a
                // `SurfaceSession`, whose Java constructor calls
                // `nativeCreate()` immediately.
                "register_android_view_SurfaceSession",
                // `ViewRootImpl.setView` (line 1318 on API 34) touches
                // `InputChannel`, whose `<clinit>` calls
                // `nativeGetFinalizer()`.
                "register_android_view_InputChannel",
                // `ViewRootImpl.setView` (line 1455 on API 34) constructs
                // `WindowInputEventReceiver`, whose `InputEventReceiver.<init>`
                // calls `nativeInit(...)`. libandroid_runtime exports the
                // registration entry point; without it the class has no
                // native implementation (W3's wall). The InputChannel it is
                // handed is the real socketpair from `addToDisplayAsUser`.
                "register_android_view_InputEventReceiver",
                // REL3I: the bound IME turns the other way round — the app is the
                // sender. `InputMethodManager.sendInputEventOnMainLooperLocked`
                // constructs `ImeInputEventSender` (an `InputEventSender`), whose
                // constructor calls `nativeInit(..., InputChannel, MessageQueue)`.
                // The image exports the registration; without it the first key
                // offered to the IME aborts the looper with
                // `UnsatisfiedLinkError: InputEventSender.nativeInit`.
                "register_android_view_InputEventSender",
                // I2: the receiver's native `dispatchInputEvent` builds the
                // Java event with the framework's own static factories
                // (`MotionEvent.obtain` / `KeyEvent.obtain`). Those method ids
                // live in the MotionEvent/KeyEvent registrations; without them
                // the first published message aborts with
                // "mid == null in CallStaticObjectMethodV from
                //  MessageQueue.nativePollOnce" (the same allowlist class of
                // wall as W4's InputEventReceiver gap).
                "register_android_view_MotionEvent",
                "register_android_view_KeyEvent",
                // `ViewRootImpl.performTraversals` -> `dispatchAttachedToWindow`
                // -> `TextView.drawableStateChanged` -> `StateListAnimator` runs
                // an `ObjectAnimator`, whose `PropertyValuesHolder` setup uses
                // the stock reflection helper natives.
                "register_android_animation_PropertyValuesHolder",
                // After the resume, `PhoneWindow.preparePanel` ->
                // `KeyCharacterMap.load` -> `InputManagerGlobal.getInputDevice`
                // -> `InputDevice.<init>` -> `KeyCharacterMap.<init>` ->
                // `nativeReadFromParcel`. Both registrations exist in the
                // image's libandroid_runtime; without them the looper throws
                // (`LOOPER_THREW`) right after `activity-resumed` (W5).
                "register_android_view_InputDevice",
                "register_android_view_KeyCharacterMap",
                "register_android_text_AndroidCharacter",
                "register_android_text_Hyphenator",
                "register_android_util_CharsetUtils",
                // W15: android.widget.Switch's constructor (and therefore any
                // ScrollView layout that inflates a Switch) calls
                // VelocityTracker.obtain() -> nativeInitialize. Without this
                // registration the Activity cannot inflate (widget-gallery).
                "register_android_view_VelocityTracker",
                // W15: SQLiteConnection's natives live in the framework
                // registration (libandroid_runtime exports it, implemented by
                // libsqlite); without it the first sqlite open is an
                // UnsatisfiedLinkError (sqlite-crud).
                "register_android_database_SQLiteConnection",
                // W15: the next sqlite wall after the connection registration:
                // the first query allocates a CursorWindow
                // (nativeCreate/nativeRead/nativeWrite). The image exports this
                // registration; without it the Activity dies on first read.
                "register_android_database_CursorWindow",
                // DisplayManagerGlobal's constructor reads OverlayProperties
                // (IDisplayManager.getOverlaySupport) even when the reply is
                // null; the CREATOR field reference runs <clinit>.
                "register_android_hardware_OverlayProperties",
                // W25: a React Native app's ReactActivityDelegate builds the
                // dev-support manager inside Activity.onCreate, and its shake
                // detector asks for SENSOR_SERVICE: SystemSensorManager.<init>
                // (line 148 on API 34) calls nativeClassInit(). The image's
                // libandroid_runtime exports
                // `register_android_hardware_SensorManager`; without it the
                // first third-party Activity dies with
                // `UnsatisfiedLinkError: No implementation found for void
                // android.hardware.SystemSensorManager.nativeClassInit()`
                // (Gaten, w25-gaten-launch).
                "register_android_hardware_SensorManager",
                // MODA11YNATIVE: an accessibility service's `takeScreenshot`
                // result is a `HardwareBuffer` the app reads with
                // `nReadHardwareBufferFromParcel`/the `nGet*` getters and uses
                // with `Bitmap.wrapHardwareBuffer`. The image's libandroid_runtime
                // exports the registration (and libui's `GraphicBuffer`
                // unflatten goes through the ACOS gralloc shim, which owns the
                // memfd), so the guest runs the REAL implementation — no
                // substitute native.
                "register_android_hardware_HardwareBuffer",
                "register_com_android_internal_util_VirtualRefBasePtr",
                // WV4 (wall 8): `android.net.LocalSocketImpl`'s socket natives.
                // The WebView provider's session transport is one `AF_UNIX`
                // stream socket per session (`results/handoff/WVB/WIRE.md` §0);
                // `AcosSessionSocket.open` calls
                // `LocalSocket.connect(new LocalSocketAddress("/run/acos/webview.sock",
                // Namespace.FILESYSTEM))`, i.e. `LocalSocketImpl.connectLocal`,
                // whose seven natives (connectLocal, bindLocal, read_native,
                // readba_native, write_native, writeba_native,
                // getPeerCredentials_native) live in the image's own
                // libandroid_runtime (`register_android_net_LocalSocketImpl`,
                // base/core/jni/android_net_LocalSocketImpl.cpp). Without the
                // registration the provider dies with `UnsatisfiedLinkError: No
                // implementation found for void
                // android.net.LocalSocketImpl.connectLocal(...)`. The image's
                // implementation is a real `connect(2)` with real errno ->
                // IOException (`socket_local_client_connect`), so the failure
                // with no broker is the honest ENOENT/ECONNREFUSED, never a
                // fabricated transport.
                "register_android_net_LocalSocketImpl",
                // CAMERA2/3: `CameraManagerGlobal.connectCameraServiceLocked()` calls
                // `CameraMetadataNative.setupGlobalVendorTagDescriptor()`,
                // `getCameraCharacteristics` unparcels `CameraMetadataNative` via
                // `libcamera_client.so` + `libcamera_metadata.so`, and
                // `OutputConfiguration(Surface)` calls `SurfaceUtils.getSurface*`.
                "register_android_hardware_camera2_CameraMetadata",
                "register_android_hardware_camera2_utils_SurfaceUtils",
        };
        String[] graphics = {"register_android_graphics_classes"};
        StringBuilder missing = new StringBuilder();
        for (String n : names) {
            int rc;
            try {
                rc = RenderNat.register(n);
            } catch (Throwable t) {
                rc = -1;
            }
            if (rc != 1) missing.append(' ').append(n).append('=').append(rc);
        }
        for (String n : graphics) {
            int rc;
            try {
                rc = RenderNat.register(n);
            } catch (Throwable t) {
                rc = -1;
            }
            if (rc != 1) missing.append(' ').append(n).append('=').append(rc);
        }
        // GLSPIKE4: android.opengl.EGL14/GLES20 natives, ONLY when a guest GLES
        // provider was selected (`ACOS_GUEST_GLES`, property `acos.guest.gles`
        // from crates/acos-art/src/gles.rs). Default OFF is inert by
        // construction: with no provider staged libEGL has no driver, and a
        // registered EGL14 would turn a catchable UnsatisfiedLinkError in an
        // app that uses it into libEGL's `couldn't find an OpenGL ES
        // implementation` SIGABRT. The image's own libandroid_runtime exports
        // the registrations (`register_android_opengl_jni_EGL14` / `_GLES20`,
        // global scope); registering runs no Java class initialiser.
        if (guestGlesSelected()) {
            for (String n : new String[] {
                    "register_android_opengl_jni_EGL14",
                    "register_android_opengl_jni_EGL15",
                    "register_android_opengl_jni_GLES20"}) {
                int rc;
                try {
                    rc = RenderNat.register(n);
                } catch (Throwable t) {
                    rc = -1;
                }
                if (rc != 1) missing.append(' ').append(n).append('=').append(rc);
            }
        }
        // Canvas.setCompatibilityVersion needs libhwui's graphics natives; this
        // is the same probe E2D used to unblock ActivityThread.systemMain().
        try {
            Class<?> hr = Class.forName("android.graphics.HardwareRenderer");
            Method m = null;
            for (Method cand : hr.getDeclaredMethods()) {
                if (cand.getName().equals("nSetIsSystemOrPersistent")) { m = cand; break; }
            }
            if (m != null) {
                m.setAccessible(true);
                m.invoke(null, Boolean.FALSE);
            }
        } catch (Throwable t) {
            missing.append(" HardwareRenderer.nSetIsSystemOrPersistent=").append(t);
        }
        System.out.println("ACOSLAUNCH MARK natives registered"
                + (missing.length() == 0 ? "" : " missing:" + missing));
    }

    /**
     * Diagnostic-only barrier: when the frame probe is enabled, let it collect
     * `PROBE_TARGET_FRAMES` real vsync callbacks before the activity transaction
     * runs. The transaction's `addToDisplayAsUser` reply is the next wall and
     * throws on the main thread, which would otherwise stop the probe after one
     * frame. No-op (and never waits) when the probe is disabled.
     */
    static void awaitFrameProbe() {
        if (sProbeFrames < 0) return;
        long deadline = System.currentTimeMillis() + 3000;
        while (sProbeFrames < PROBE_TARGET_FRAMES && System.currentTimeMillis() < deadline) {
            try {
                Thread.sleep(5);
            } catch (InterruptedException ignored) {
            }
        }
        System.out.println("ACOSLAUNCH MARK vsync-probe frames=" + sProbeFrames
                + " target=" + PROBE_TARGET_FRAMES);
        if (sProbeFrames > 0) {
            StringBuilder deltas = new StringBuilder();
            boolean monotonic = true;
            long previous = 0;
            for (int i = 0; i < sProbeFrames; i++) {
                if (i > 0) deltas.append(',');
                deltas.append(sProbeDeltas[i]);
                if (i > 0 && sProbeDeltas[i] <= 0) monotonic = false;
                previous += sProbeDeltas[i];
            }
            boolean contiguous = sProbeLast - sProbeFirst == previous;
            System.out.println("ACOSLAUNCH MARK vsync-probe-summary first=" + sProbeFirst
                    + " last=" + sProbeLast + " monotonic=" + monotonic
                    + " contiguous=" + contiguous + " deltasNanos=" + deltas);
        }
        System.out.flush();
    }

    /** Frame-clock probe: real vsync callbacks on the main looper. */
    static void postFrameProbe() {
        try {
            final android.view.Choreographer c = android.view.Choreographer.getInstance();
            sProbeFrames = 0;
            c.postFrameCallback(new android.view.Choreographer.FrameCallback() {
                int seen;
                long previous;

                @Override
                public void doFrame(long frameTimeNanos) {
                    seen++;
                    long delta = previous == 0 ? 0 : frameTimeNanos - previous;
                    previous = frameTimeNanos;
                    if (seen == 1) sProbeFirst = frameTimeNanos;
                    sProbeLast = frameTimeNanos;
                    if (seen <= PROBE_TARGET_FRAMES) sProbeDeltas[seen - 1] = delta;
                    sProbeFrames = seen;
                    System.out.println("ACOSLAUNCH MARK vsync-frame count=" + seen
                            + " frameTimeNanos=" + frameTimeNanos
                            + " deltaNanos=" + delta);
                    if (seen < PROBE_TARGET_FRAMES) c.postFrameCallback(this);
                }
            });
            System.out.println("ACOSLAUNCH MARK choreographer-ready");
        } catch (Throwable t) {
            System.out.println("ACOSLAUNCH MARK choreographer-failed " + t);
        }
    }

    // ------------------------------------------------- bindApplication bytes

    /**
     * Marshal {@code IApplicationThread.bindApplication} exactly as the real
     * AMS does: every argument is written by the framework class that owns its
     * {@code writeToParcel}, in API-34 AIDL order. The request never touches a
     * hand-written codec; only the transport is ACOS's.
     */
    static ApplicationInfo buildAppInfo() throws Throwable {
        ApplicationInfo ai = new ApplicationInfo();
        ai.packageName = sPkg;
        ai.sourceDir = sApk;
        ai.publicSourceDir = sApk;
        ai.dataDir = "/data/app-data/" + sPkg;
        // W17: the per-run extracted `lib/<abi>` tree (installd's
        // `nativeLibraryDir`); `LoadedApk` adds it to the class loader's
        // native search path. An empty directory is bound too, so the path
        // always exists for apps without native code.
        ai.nativeLibraryDir = "/data/app-lib/x86_64";
        // W17: the APK's own Application class. Empty here means the base
        // `android.app.Application` (the platform default), which is what the
        // guest ran for every APK before this line existed.
        if (!sAppClass.isEmpty()) ai.className = sAppClass;
        // W24: `PackageParser`'s `android:label` fields. `ApplicationInfo.loadLabel`
        // reads `nonLocalizedLabel` first, then `labelRes`; with both unset the
        // activity title walk ends at `ActivityInfo.name` (W18 #1).
        ai.labelRes = sAppLabelRes;
        if (!sAppLabelText.isEmpty()) ai.nonLocalizedLabel = sAppLabelText;
        // W17: the manifest's `android:theme`, the resource `PackageParser`
        // copies into the activity's `ActivityInfo.theme`; AppCompat activities
        // abort without it (`Theme.AppCompat` check in createSubDecor).
        ai.theme = sAppTheme;
        ai.processName = sPkg;
        ai.uid = android.os.Process.myUid();
        ai.minSdkVersion = 26;
        ai.targetSdkVersion = 34;
        // W31: the flags `PackageParser.parseBaseApplication` derives from the
        // manifest (`acos-apk::manifest::Manifest::application_flags`), plus
        // PMS's post-install `FLAG_INSTALLED`. The host supplies them; before
        // W31 this line hardcoded `0x4 | 0x800000`, so
        // `FLAG_USES_CLEARTEXT_TRAFFIC` (1<<27) was missing for an APK whose
        // manifest sets `android:usesCleartextTraffic="true"` and
        // `NetworkSecurityPolicy` denied RN's `http://localhost:8081` probe
        // (W28 §6.1).
        // FLAG_HARDWARE_ACCELERATED is deliberately absent: the platform default
        // for the software `ViewRootImpl` path is the only drawing route a
        // surface-less ACOS window can serve at all (P4b.md §4); the host clears
        // the bit the manifest asks for and the guest never re-adds it.
        if (sAppFlags >= 0) {
            ai.flags = sAppFlags;
        } else {
            // FLAG_HAS_CODE(0x4) | FLAG_INSTALLED(0x800000)
            ai.flags = 0x4 | 0x800000;
        }
        System.out.println("ACOSLAUNCH MARK app-flags flags=0x"
                + Integer.toHexString(ai.flags)
                + " source=" + (sAppFlags >= 0 ? sAppFlagsSource : "default"));
        System.out.flush();
        // No `FLAG_HARDWARE_ACCELERATED`: the platform default for an app that
        // does not ask for it is the software ViewRootImpl path, which is the
        // only drawing route a surface-less ACOS window can serve at all
        // (P4b.md §4).
        set(ai, "deviceProtectedDataDir", ai.dataDir);
        set(ai, "credentialProtectedDataDir", ai.dataDir);
        // K3d: the `<application>` `<meta-data>` `PackageParser` stores off to
        // the side (`Package.mAppMetaData`). `ActivityThread` hands
        // `data.appInfo` to `LoadedApk.makeApplication`, so an app that reads
        // `context.getApplicationInfo().metaData` sees its own declaration
        // instead of `null`. The host parses it with the same model PMS answers
        // `getApplicationInfo(GET_META_DATA)` from.
        String appMeta = System.getenv("ACOSLAUNCH_APP_META");
        if (appMeta != null) ai.metaData = metaBundle(appMeta);
        set(ai, "compileSdkVersion", 34);
        set(ai, "appComponentFactory", "android.app.AppComponentFactory");
        set(ai, "enabled", Boolean.TRUE);
        set(ai, "enabledSetting", 0);
        return ai;
    }

    /**
     * W47: the process launch `Configuration`, with the stock screen-layout
     * bits.
     *
     * `ResourcesManager.applyConfigurationToResourcesLocked` hands this object
     * to `AssetManager.nativeSetConfiguration`. Leaving `screenLayout` at its
     * default `0` leaves the resource size qualifier undefined, and
     * `ResTable_config::match` then accepts the `xlarge` variants:
     * `@dimen/dialog_min_width_minor` resolves to its `(xlarge)` 72% instead
     * of the default 95%. A `WRAP_CONTENT` AlertDialog therefore measures
     * 776 px instead of the stock 1024 px (940 px content after insets).
     *
     * The value is the host's WMS reference configuration
     * (`ACOSLAUNCH_SCREEN_LAYOUT`); the fallback is the measured
     * `ldltr nrml long` for 411x842 dp. This is the framework's own
     * `Configuration.reduceScreenLayout` outcome, not a per-window rule.
     */
    /**
     * The panel the guest boots at: the ACOS reference device by default, or the
     * reference *phone* when the caller forwards its geometry
     * (`ACOSLAUNCH_WIDTH/_PANEL_HEIGHT/_DENSITY/_STATUS_INSET/_NAV_INSET`).
     *
     * <p>The engine forwards those variables from `ACOS_ART_*` so a verification
     * run can be compared pixel-for-pixel with a physical device at the same
     * density, insets and window size — the values the framework derives
     * (`Configuration.screenWidthDp`, `DisplayInfo.appWidth`, the window frame)
     * are computed from them rather than hardcoded, exactly as
     * `DisplayMetrics`/`Configuration` do on a device.
     */
    static int PANEL_W = envInt("ACOSLAUNCH_WIDTH", 1080);
    static int PANEL_H = envInt("ACOSLAUNCH_PANEL_HEIGHT", 2400);
    static int DENSITY_DPI = envInt("ACOSLAUNCH_DENSITY", 420);
    /**
     * The display's nominal refresh rate, the same value `acos-services`'
     * `DisplayInfo` serves (`60.000004` is what the API-34 emulator reports).
     * `Display.getRefreshRate()` is what the framework samples time against
     * (`AccessibilityService.calculateGestureSampleTimeMs`, `Choreographer`
     * frame math), so the guest must report a real rate, never `0`.
     */
    static final float REFRESH_RATE_HZ = 60.000004f;
    static int STATUS_INSET = envInt("ACOSLAUNCH_STATUS_INSET", 128);
    static int NAV_INSET = envInt("ACOSLAUNCH_NAV_INSET", 63);
    /**
     * W7: the app-window inset model the host WMS is using. `carve` is the
     * pre-W7 shim (the app area is the display minus the bars); `report` is
     * AOSP's model (the app window is the full display and the WMS reports the
     * bar insets, which DecorView or the app's own padding consumes).
     *
     * <p>The guest must derive the same `DisplayInfo.appWidth/appHeight` and
     * `Configuration` size as the WMS frame, otherwise a `report` run would
     * still tell the app it owns only the carved rectangle.
     */
    static String WM_INSETS = env("ACOSLAUNCH_WM_INSETS", "carve");
    static boolean REPORT_INSETS = "report".equalsIgnoreCase(WM_INSETS);
    /** Device pixels the app area occupies (`appWidth`/`appHeight`). */
    static int APP_W = PANEL_W;
    static int APP_H = REPORT_INSETS ? PANEL_H : PANEL_H - STATUS_INSET - NAV_INSET;

    static String env(String name, String fallback) {
        try {
            String v = System.getenv(name);
            if (v != null && v.trim().length() > 0) return v.trim();
        } catch (Throwable ignored) {
        }
        return fallback;
    }

    static int envInt(String name, int fallback) {
        try {
            String v = System.getenv(name);
            if (v != null && v.trim().length() > 0) return Integer.parseInt(v.trim());
        } catch (Throwable ignored) {
        }
        return fallback;
    }

    /** dp for a device-pixel count at this density (framework rounding: px/density). */
    static int dp(int px) {
        return (int) Math.round(px * 160.0 / (double) DENSITY_DPI);
    }

    static Configuration displayConfiguration() {
        Configuration cfg = new Configuration();
        cfg.densityDpi = DENSITY_DPI;
        cfg.screenWidthDp = dp(PANEL_W);
        cfg.screenHeightDp = dp(PANEL_H);
        cfg.smallestScreenWidthDp = dp(Math.min(PANEL_W, PANEL_H));
        cfg.orientation = Configuration.ORIENTATION_PORTRAIT;
        cfg.fontScale = 1.0f;
        cfg.setLocales(android.os.LocaleList.forLanguageTags("en-US"));
        int layout = Configuration.SCREENLAYOUT_LAYOUTDIR_LTR
                | Configuration.SCREENLAYOUT_SIZE_NORMAL
                | Configuration.SCREENLAYOUT_LONG_YES;
        String env = System.getenv("ACOSLAUNCH_SCREEN_LAYOUT");
        if (env != null) {
            try {
                layout = Integer.decode(env.trim());
            } catch (NumberFormatException e) {
                System.out.println("ACOSLAUNCH MARK bad-screen-layout " + env);
            }
        }
        cfg.screenLayout = layout;
        return cfg;
    }

    static byte[] buildBindApplicationRequest() throws Throwable {
        ApplicationInfo ai = buildAppInfo();

        Object providerList = providerList(ai);

        Configuration cfg = displayConfiguration();

        Class<?> ci = Class.forName("android.content.res.CompatibilityInfo");
        Field df = ci.getDeclaredField("DEFAULT_COMPATIBILITY_INFO");
        df.setAccessible(true);
        Object compat = df.get(null);

        Bundle instrArgs = new Bundle();
        instrArgs.putString("apk", sApk);
        instrArgs.putString("package", sPkg);
        // AMS sends a real, non-null Bundle here; `handleSetCoreSettings` reads
        // keys unconditionally (an empty Bundle answers every key with null).
        Bundle coreSettings = new Bundle();
        Parcel p = Parcel.obtain();
        try {
            p.writeInterfaceToken("android.app.IApplicationThread");
            p.writeString(sPkg);                                   // packageName
            p.writeTypedObject(ai, 0);                             // ApplicationInfo
            p.writeString(sPkg);                                   // processName
            p.writeString(null);                                   // sdkSandbox volume
            p.writeTypedObject((android.os.Parcelable) providerList, 0); // ProviderInfoList
            // P4b: the real framework creates the Instrumentation. With a null
            // `instrumentationName`, `handleBindApplication` takes the else
            // branch: `mInstrumentation = new Instrumentation();
            // mInstrumentation.basicInit(this)`, then
            // `callApplicationOnCreate(app)`. Nothing in the APK or the guest
            // is involved, and no class loader seam is needed.
            p.writeTypedObject(null, 0);                           // instrumentationName
            p.writeTypedObject(null, 0);                           // ProfilerInfo
            p.writeTypedObject(instrArgs, 0);                      // instrumentationArgs
            p.writeStrongBinder(null);                             // IInstrumentationWatcher
            p.writeStrongBinder(null);                             // IUiAutomationConnection
            p.writeInt(0);                                         // debugMode
            p.writeInt(0);                                         // enableBinderTracking
            p.writeInt(0);                                         // trackAllocation
            p.writeInt(0);                                         // isRestrictedBackupMode
            p.writeInt(0);                                         // persistent
            p.writeTypedObject(cfg, 0);                            // Configuration
            p.writeTypedObject((android.os.Parcelable) compat, 0);      // CompatibilityInfo
            p.writeInt(0);                                         // services Map (empty)
            p.writeTypedObject(coreSettings, 0);                   // coreSettings
            p.writeString("acos");                                 // buildSerial
            p.writeTypedObject(null, 0);                           // AutofillOptions
            p.writeTypedObject(null, 0);                           // ContentCaptureOptions
            p.writeLongArray(new long[0]);                         // disabledCompatChanges
            p.writeTypedObject(null, 0);                           // SharedMemory
            p.writeLong(0);                                        // startRequestedElapsed
            p.writeLong(0);                                        // startRequestedUptime
            byte[] raw = p.marshall();
            System.out.println("ACOSLAUNCH MARK bind-request-marshalled appInfo=("
                    + ai.packageName + " sourceDir=" + ai.sourceDir + " uid=" + ai.uid + ") bytes="
                    + raw.length);
            return raw;
        } finally {
            p.recycle();
        }
    }

    /**
     * K3c: the app's own manifest-declared content providers, in the shape
     * {@code ActivityThread.handleBindApplication} consumes.
     *
     * <p>A real device's AMS fills {@code BindApplicationData.providers} from
     * {@code generateApplicationProvidersLocked} — every {@code <provider>} of
     * the package whose {@code android:process} resolves to this process. The
     * framework then runs {@code installContentProviders}: for each entry it
     * instantiates the class through {@code AppComponentFactory},
     * {@code attachInfo} (which runs {@code ContentProvider.onCreate}) and
     * reports the holder back through
     * {@code IActivityManager.publishContentProviders}. That is the stock order:
     * providers come up <em>after</em> {@code Application.attachBaseContext} and
     * <em>before</em> {@code Application.onCreate}, and nothing here calls a
     * provider by hand.
     *
     * <p>Before K3c this returned an empty list, so androidx.startup and
     * WorkManager's {@code InitializationProvider} never existed and
     * {@code ProcessLifecycleInitializer} could not be initialized lazily
     * (io.element.android.x, K2 §6).
     *
     * <p>The spec is host-parsed from the APK's AndroidManifest.xml
     * ({@code acos-services::pms::PmsState}, the same model PMS answers
     * {@code getProviderInfo} from): one record per provider, newline
     * separated, fields {@code class|authorities|permission|exported|grantUri}.
     * The host
     * already dropped providers whose {@code android:process} is not this
     * process, exactly as {@code AMS.generateApplicationProvidersLocked} does.
     * Nothing is invented: a provider the manifest does not declare cannot
     * appear here.
     */
    static Object providerList(ApplicationInfo ai) throws Throwable {
        java.util.List<Object> list = new java.util.ArrayList<Object>();
        String spec = System.getenv("ACOSLAUNCH_PROVIDERS");
        if (spec != null && spec.length() > 0) {
            for (String rec : spec.split("\n")) {
                if (rec.length() == 0) continue;
                String[] f = rec.split("\\|", -1);
                if (f.length < 4) continue;
                android.content.pm.ProviderInfo pi = new android.content.pm.ProviderInfo();
                pi.name = f[0];
                pi.packageName = sPkg;
                pi.authority = f[1];
                pi.readPermission = f[2].length() == 0 ? null : f[2];
                pi.writePermission = f[2].length() == 0 ? null : f[2];
                pi.exported = "1".equals(f[3]);
                pi.grantUriPermissions = f.length > 4 && "1".equals(f[4]);
                pi.processName = sPkg;
                pi.applicationInfo = ai;
                pi.enabled = true;
                pi.initOrder = 0;
                pi.isSyncable = false;
                pi.multiprocess = false;
                // K3d: the provider's own `<meta-data>`, host-parsed from the
                // APK's manifest by the same `PackageParser` model PMS answers
                // `getProviderInfo`/`resolveContentProvider` from. Empty when
                // the manifest declares none: the provider still has a
                // metadata map, it is just empty.
                pi.metaData = f.length > 5
                        ? metaBundle(f[5])
                        : new android.os.Bundle();
                list.add(pi);
            }
        }
        System.out.println("ACOSLAUNCH MARK providers count=" + list.size());
        System.out.flush();
        Class<?> cls = Class.forName("android.content.pm.ProviderInfoList");
        Method fromList = cls.getMethod("fromList", java.util.List.class);
        return fromList.invoke(null, list);
    }

    /**
     * K3d: decode the host's `<meta-data>` field encoding
     * (`name=type+value` entries joined with `;`, `|`, `;`, `=` and newlines
     * escaped as `\|`, `\s`, `\e`, `\n`, and backslash as `\\`).
     *
     * <p>The value types are the four `Bundle` types
     * `PackageParser.parseMetaData` stores: `putString` (`s`), `putInt` (`i`,
     * which is also how an `android:resource` reference id is stored),
     * `putBoolean` (`b`) and `putFloat` (`f`). No other type reaches a bundle
     * on a device either.
     */
    static android.os.Bundle metaBundle(String spec) {
        android.os.Bundle bundle = new android.os.Bundle();
        if (spec == null || spec.length() == 0) return bundle;
        for (String entry : spec.split(";", -1)) {
            if (entry.length() == 0) continue;
            int eq = entry.indexOf('=');
            if (eq <= 0 || eq + 2 > entry.length()) continue;
            String key = metaUnescape(entry.substring(0, eq));
            char kind = entry.charAt(eq + 1);
            String raw = metaUnescape(entry.substring(eq + 2));
            try {
                switch (kind) {
                    case 's': bundle.putString(key, raw); break;
                    case 'i': bundle.putInt(key, Integer.parseInt(raw)); break;
                    case 'b': bundle.putBoolean(key, "1".equals(raw)); break;
                    case 'f': bundle.putFloat(key, Float.parseFloat(raw)); break;
                    default:
                        System.out.println("ACOSLAUNCH WARN meta-data type " + kind
                                + " is not a Bundle type; dropped (" + key + ")");
                }
            } catch (NumberFormatException e) {
                System.out.println("ACOSLAUNCH WARN meta-data value for " + key
                        + " is not a " + kind + ": " + raw);
            }
        }
        return bundle;
    }

    static String metaUnescape(String text) {
        StringBuilder out = new StringBuilder(text.length());
        for (int i = 0; i < text.length(); i++) {
            char c = text.charAt(i);
            if (c != '\\' || i + 1 >= text.length()) {
                out.append(c);
                continue;
            }
            char n = text.charAt(++i);
            switch (n) {
                case 'p': out.append('|'); break;
                case 's': out.append(';'); break;
                case 'e': out.append('='); break;
                case 'n': out.append('\n'); break;
                default: out.append(n); break;
            }
        }
        return out.toString();
    }

    // ------------------------------------------------------ conformance probe

    /**
     * Diagnostic (`ACOSLAUNCH_PROBE=1`): have the framework marshal a real
     * {@code DisplayInfo} with the launch display values and print the bytes,
     * then read them back with the framework's own CREATOR. The peer's
     * {@code IDisplayManager.getDisplayInfo} reply must decode with the same
     * reader; a mismatch here is a parcel-codec bug, not a framework one.
     */
    static void probeDisplayInfoParcel() {
        try {
            Class<?> cls = Class.forName("android.view.DisplayInfo");
            Object info = cls.getDeclaredConstructor().newInstance();
            for (String[] kv : new String[][]{
                    {"displayId", "0"}, {"logicalWidth", Integer.toString(PANEL_W)},
                    {"logicalHeight", Integer.toString(PANEL_H)}, {"appWidth", Integer.toString(APP_W)},
                    {"appHeight", Integer.toString(APP_H)}, {"name", "Built-in Screen"},
                    {"flags", "0"}, {"type", "0"},
                    {"rotation", "0"}, {"modeId", "0"},
                    {"logicalDensityDpi", Integer.toString(DENSITY_DPI)}, {"state", "2"},
                    {"colorMode", "0"}, {"installOrientation", "0"}}) {
                String f = kv[0];
                java.lang.reflect.Field fd = cls.getField(f);
                if (fd.getType() == int.class) fd.setInt(info, Integer.parseInt(kv[1]));
                else if (fd.getType() == float.class) fd.setFloat(info, Float.parseFloat(kv[1]));
                else if (fd.getType() == String.class) fd.set(info, kv[1]);
            }
            // Sentinel map: every public int field gets a unique value so the
            // marshalled offsets identify the framework's field order.
            StringBuilder map = new StringBuilder();
            int sentinel = 1001;
            for (java.lang.reflect.Field fd : cls.getFields()) {
                if (fd.getType() != int.class) continue;
                if (java.lang.reflect.Modifier.isStatic(fd.getModifiers())) continue;
                fd.setInt(info, sentinel);
                map.append(' ').append(fd.getName()).append('=').append(sentinel);
                sentinel++;
            }
            System.out.println("ACOSLAUNCH PROBE displayinfo-sentinels" + map);
            Parcel p = Parcel.obtain();
            ((android.os.Parcelable) info).writeToParcel(p, 0);
            byte[] raw = p.marshall();
            p.setDataPosition(0);
            StringBuilder hex = new StringBuilder();
            for (byte b : raw) hex.append(String.format("%02x", b));
            System.out.println("ACOSLAUNCH PROBE displayinfo-write len=" + raw.length + " hex=" + hex);
            android.os.Parcelable back = cls.getField("CREATOR").get(null) instanceof android.os.Parcelable.Creator
                    ? (android.os.Parcelable) ((android.os.Parcelable.Creator<?>) cls.getField("CREATOR").get(null)).createFromParcel(p)
                    : null;
            System.out.println("ACOSLAUNCH PROBE displayinfo-read-back ok=" + (back != null)
                    + " w=" + cls.getField("logicalWidth").getInt(back));
            p.recycle();
        } catch (Throwable t) {
            System.out.println("ACOSLAUNCH PROBE displayinfo failed " + t);
        }
    }

    /**
     * Replies for `getDisplayInfo` / `getInstrumentationInfo(AsUser)`, marshalled
     * by the framework's own `writeToParcel`. Both parcels are complete AIDL
     * replies: the exception header (`0`) then `writeTypedObject`.
     *
     * <p>Why the framework writes them: `acos-services`' DisplayInfo codec still
     * describes a pre-API-34 layout (the real writer has two `writeParcelable`s,
     * the supported-mode array, address/deviceProductInfo, uid/model, …), and
     * its PackageManager deliberately does not serve `<instrumentation>`. The
     * bytes come from the framework classes, as system_server does; the values
     * are the display ACOS serves and the instrumentation component this launch
     * harness declares (the same one the bind request names).
     */
    static byte[] buildDisplayInfoReply() throws Throwable {
        Class<?> cls = Class.forName("android.view.DisplayInfo");
        Object info = cls.getDeclaredConstructor().newInstance();
        setField(cls, info, "displayId", 0);
        setField(cls, info, "logicalWidth", PANEL_W);
        setField(cls, info, "logicalHeight", PANEL_H);
        setField(cls, info, "appWidth", APP_W);
        setField(cls, info, "appHeight", APP_H);
        setField(cls, info, "smallestNominalAppWidth", APP_W);
        setField(cls, info, "smallestNominalAppHeight", APP_H);
        setField(cls, info, "largestNominalAppWidth", APP_W);
        setField(cls, info, "largestNominalAppHeight", APP_H);
        setField(cls, info, "name", "Built-in Screen");
        setField(cls, info, "flags", 0);
        setField(cls, info, "type", 1);
        setField(cls, info, "rotation", 0);
        setField(cls, info, "modeId", 1);
        setField(cls, info, "defaultModeId", 1);
        // MPROC7: a real display always lists its modes. Without this array the
        // framework's `DisplayInfo.findMode()` throws `IllegalStateException:
        // Unable to locate mode id=1,supportedModes=[]` as soon as something
        // initialises hardware rendering (`HardwareRenderer.ProcessInitializer
        // .initDisplayInfo` from `ViewRootImpl.setView`) — which is exactly
        // what an activity with `ActivityInfo.FLAG_HARDWARE_ACCELERATED` does.
        Class<?> modeCls = Class.forName("android.view.Display$Mode");
        java.lang.reflect.Constructor<?> modeCtor = modeCls.getDeclaredConstructor(
                int.class, int.class, int.class, float.class);
        modeCtor.setAccessible(true);
        // MERGEMPROC: the Mode carries the display's **real** refresh rate, not a
        // rounded 60.0. `DisplayInfo.getRefreshRate()` answers from this mode
        // (`supportedModes[0].getRefreshRate()`), and an accessibility service
        // samples `dispatchGesture` against it — a 60.0 here made the app report
        // `refresh_rate=60.0` where the framework's own constant is 60.000004.
        Object mode = modeCtor.newInstance(1, PANEL_W, PANEL_H, REFRESH_RATE_HZ);
        Object modes = java.lang.reflect.Array.newInstance(modeCls, 1);
        java.lang.reflect.Array.set(modes, 0, mode);
        setField(cls, info, "supportedModes", modes);
        setField(cls, info, "logicalDensityDpi", DENSITY_DPI);
        setField(cls, info, "state", 2);
        setField(cls, info, "colorMode", 0);
        setField(cls, info, "installOrientation", 0);
        setField(cls, info, "renderFrameRate", REFRESH_RATE_HZ);
        setField(cls, info, "refreshRate", REFRESH_RATE_HZ);
        // `DisplayInfo.getRefreshRate()` (the image's own bytecode) reads
        // `refreshRateOverride > 0 ? it : (supportedModes.length == 0 ? 0 :
        // getMode().getRefreshRate())`, so the one mode above is also what makes
        // a non-zero rate available: with an EMPTY `supportedModes` every app
        // read `0 Hz`, and `AccessibilityService.calculateGestureSampleTimeMs`
        // turned that into `(int)(1000 / 0.0f) == Integer.MAX_VALUE` — a gesture
        // then had only its key-point steps (press and release, no `MOVE`s), so
        // a swipe never scrolled anything. A real display always carries the
        // mode it is driving.
        setField(cls, info, "refreshRateOverride", 0.0f);
        Parcel p = Parcel.obtain();
        try {
            p.writeInt(0); // AIDL reply: no exception
            p.writeTypedObject((android.os.Parcelable) info, 0);
            return p.marshall();
        } finally {
            p.recycle();
        }
    }

    /** The `getInstrumentationInfo(AsUser)` reply, written by the framework. */
    static byte[] buildInstrumentationInfoReply() throws Throwable {
        Class<?> cls = Class.forName("android.content.pm.InstrumentationInfo");
        Object info = cls.getDeclaredConstructor().newInstance();
        String name = "dev.acos.art.LaunchMain$Instr";
        String dataDir = "/data/app-data/" + sPkg;
        setField(cls, info, "name", name);
        setField(cls, info, "packageName", sPkg);
        setField(cls, info, "targetPackage", sPkg);
        setField(cls, info, "sourceDir", sApk);
        setField(cls, info, "publicSourceDir", sApk);
        setField(cls, info, "dataDir", dataDir);
        setField(cls, info, "deviceProtectedDataDir", dataDir);
        setField(cls, info, "credentialProtectedDataDir", dataDir);
        setField(cls, info, "nativeLibraryDir", "/data/app-lib/x86_64");
        Parcel p = Parcel.obtain();
        try {
            p.writeInt(0); // AIDL reply: no exception
            p.writeTypedObject((android.os.Parcelable) info, 0);
            return p.marshall();
        } finally {
            p.recycle();
        }
    }

    // ------------------------------------------- the AMS step: ClientTransaction

    /**
     * The `ActivityInfo` a real `ActivityRecord` carries for this launch. The
     * values come from the APK's manifest (name, targetSdk) or are the platform
     * defaults `ActivityThread` itself would use for an unset field.
     */
    static Object buildActivityInfo(ApplicationInfo appInfo) throws Throwable {
        return buildActivityInfoNamed(appInfo, sActivity);
    }

    /**
     * W38: the `ActivityInfo` for one class of the launched package, so the
     * second-Activity launch transaction carries the target's own name instead
     * of the launcher entry's. Fields the manifest would supply for the target
     * (theme, label, launchMode) keep the platform defaults the launcher already
     * uses when the manifest does not set them.
     */
    static Object buildActivityInfoNamed(ApplicationInfo appInfo, String activity) throws Throwable {
        Class<?> cls = Class.forName("android.content.pm.ActivityInfo");
        Object ai = cls.getDeclaredConstructor().newInstance();
        setFieldForce(cls, ai, "applicationInfo", appInfo);
        setFieldForce(cls, ai, "name", activity);
        setFieldForce(cls, ai, "packageName", sPkg);
        setFieldForce(cls, ai, "taskAffinity", sPkg);
        boolean first = activity.equals(sActivity);
        setFieldForce(cls, ai, "targetActivity",
                first && !sTargetActivity.isEmpty() ? sTargetActivity : null);
        setFieldForce(cls, ai, "permission", null);
        setFieldForce(cls, ai, "launchMode", 0);
        setFieldForce(cls, ai, "theme", sAppTheme); // manifest theme; 0 = platform default
        // W24: the activity's own `android:label` (unset when the manifest has
        // none, so `loadLabel` falls through to the application label exactly
        // as stock `ActivityInfo.loadLabel` does).
        setFieldForce(cls, ai, "labelRes", sActivityLabelRes);
        setFieldForce(cls, ai, "nonLocalizedLabel",
                sActivityLabelText.isEmpty() ? null : sActivityLabelText);
        setFieldForce(cls, ai, "flags", 0);          // no FLAG_HARDWARE_ACCELERATED
        setFieldForce(cls, ai, "screenOrientation", -1);
        // ROT2: the manifest's `android:configChanges` bitmask (resolved by
        // `acos_services::manifest_resolve`, forced MCC/MNC included), not 0:
        // `ActivityRecord.getRealConfigChanged()` compares exactly this field
        // when a display rotation has to decide re-create vs
        // `onConfigurationChanged`.
        setFieldForce(cls, ai, "configChanges", (int) envLong("ACOSLAUNCH_CONFIG_CHANGES", 0));
        setFieldForce(cls, ai, "softInputMode", 0);
        setFieldForce(cls, ai, "uiOptions", 0);
        setFieldForce(cls, ai, "exported", true);
        setFieldForce(cls, ai, "enabled", true);
        return ai;
    }

    /**
     * Marshal `IApplicationThread.scheduleTransaction(ClientTransaction)` the way
     * AMS does: one `ClientTransaction` holding the `LaunchActivityItem` (a
     * callback) and the `ResumeActivityItem` (the final lifecycle state),
     * written by the framework's own `writeToParcel`. Only the transport is
     * ACOS's.
     *
     * <p>Every framework type on this path is `@hide`
     * (`android.app.servertransaction.*`), so it is reached by reflection — the
     * same way `ActivityThread`'s private members already are. The item
     * factories are called with arguments matched to the **image's** parameter
     * list (read from the method at runtime), not to a source tree: this image
     * is API 34 (`UE1A.230829.050`), whose `LaunchActivityItem.obtain` predates
     * the `android-14.0.0_r75` shape (no `ActivityWindowInfo`, no
     * `SceneTransitionInfo`, `ActivityOptions` instead, token carried by the
     * `ClientTransaction` itself).
     */
    static byte[] buildScheduleTransactionRequest(IBinder appThread, IBinder token, Object app)
            throws Throwable {
        Object client = iatClient(appThread);
        Object tx = buildLaunchTransaction(client, token, app, sActivity);
        Parcel p = Parcel.obtain();
        try {
            p.writeInterfaceToken("android.app.IApplicationThread");
            p.writeTypedObject((android.os.Parcelable) tx, 0);
            System.out.println("ACOSLAUNCH MARK transaction-marshalled items=launch+resume activity="
                    + sActivity + " bytes=" + p.dataSize());
            return p.marshall();
        } finally {
            p.recycle();
        }
    }

    /** The real `IApplicationThread` interface object for the local app thread. */
    static Object iatClient(IBinder appThread) throws Throwable {
        Class<?> stub = Class.forName("android.app.IApplicationThread$Stub");
        return stub.getMethod("asInterface", IBinder.class).invoke(null, appThread);
    }

    /**
     * Deliver one stock `ClientTransaction` to the app thread through the real
     * `IApplicationThread.scheduleTransaction` on the local binder. No parcel is
     * built: binders (the activity token) travel as objects, and the framework's
     * own `TransactionExecutor` runs the items on the main thread.
     */
    static void scheduleLocal(Object tx) throws Throwable {
        Class<?> iatCls = Class.forName("android.app.IApplicationThread");
        Class<?> ctxCls = Class.forName("android.app.servertransaction.ClientTransaction");
        java.lang.reflect.Method m = iatCls.getMethod("scheduleTransaction", ctxCls);
        m.setAccessible(true);
        m.invoke(sClient, tx);
    }

    /** The current and override `Configuration` a launch item carries. */
    static Configuration[] currentAndOverrideConfig(Object app) throws Throwable {
        Configuration base = displayConfiguration();
        Configuration curConfig = new Configuration(base);
        try {
            android.content.Context baseCtx = (android.content.Context) get(app, "mBase");
            if (baseCtx != null) curConfig = new Configuration(baseCtx.getResources().getConfiguration());
        } catch (Throwable t) {
            System.out.println("ACOSLAUNCH MARK cur-config-fallback " + t);
        }
        Configuration override = new Configuration();
        // An empty delta is not a zeroed Configuration: `Configuration.updateFrom`
        // only applies set fields, so an empty one is a no-op overlay.
        override.setTo(new Configuration());
        return new Configuration[]{curConfig, override};
    }

    /**
     * LAUNCHEXTRAS: the host-built launch `Intent`, unmarshalled through the
     * framework's own `Intent.CREATOR`.
     *
     * <p>ADR-0029 amendment: the host builds launch transactions. The caller's
     * `intent_action`, `data` and typed extras are written by the host's
     * `Intent.writeToParcel` encoder (byte-compared with real Java) and delivered
     * here as hex; this method only unmarshals them. `null` when the caller
     * declared nothing, in which case the guest builds the stock
     * ACTION_MAIN/CATEGORY_LAUNCHER Intent exactly as before.
     */
    static android.content.Intent hostLaunchIntent() throws Throwable {
        String hex = System.getenv("ACOSLAUNCH_INTENT_BYTES");
        if (hex == null || hex.isEmpty()) return null;
        int n = hex.length() / 2;
        byte[] bytes = new byte[n];
        for (int i = 0; i < n; i++) {
            bytes[i] = (byte) Integer.parseInt(hex.substring(i * 2, i * 2 + 2), 16);
        }
        Parcel p = Parcel.obtain();
        try {
            p.unmarshall(bytes, 0, bytes.length);
            p.setDataPosition(0);
            android.content.Intent intent =
                    android.content.Intent.CREATOR.createFromParcel(p);
            System.out.println("ACOSLAUNCH MARK launch-intent-unmarshalled action="
                    + intent.getAction() + " data=" + intent.getDataString()
                    + " component=" + intent.getComponent()
                    + " extras=" + intent.getExtras() + " bytes=" + bytes.length);
            System.out.flush();
            return intent;
        } finally {
            p.recycle();
        }
    }

    /**
     * The `ClientTransaction` a launch carries: the framework's
     * `LaunchActivityItem` callback plus the `ResumeActivityItem` lifecycle
     * request, keyed on `token` — the same shape `LaunchMain.watch` delivered
     * for the first Activity and `startActivity` now delivers for the next one.
     */
    static Object buildLaunchTransaction(Object client, IBinder token, Object app, String activity)
            throws Throwable {
        ApplicationInfo appInfo = buildAppInfo();
        Object activityInfo = buildActivityInfoNamed(appInfo, activity);
        Configuration[] cfg = currentAndOverrideConfig(app);

        android.content.Intent intent = hostLaunchIntent();
        if (intent == null) {
            intent = new android.content.Intent(android.content.Intent.ACTION_MAIN);
            intent.addCategory(android.content.Intent.CATEGORY_LAUNCHER);
            intent.setComponent(new ComponentName(sPkg, activity));
            intent.setFlags(android.content.Intent.FLAG_ACTIVITY_NEW_TASK);
        }
        return buildLaunchTransactionForIntent(client, token, app, intent, activityInfo, cfg);
    }

    static Object buildLaunchTransactionForIntent(Object client, IBinder token, Object app,
            android.content.Intent intent, Object activityInfo, Configuration[] cfg) throws Throwable {
        Class<?> ctxCls = Class.forName("android.app.servertransaction.ClientTransaction");
        Class<?> iatCls = Class.forName("android.app.IApplicationThread");
        Class<?> launchCls = Class.forName("android.app.servertransaction.LaunchActivityItem");
        Class<?> resumeCls = Class.forName("android.app.servertransaction.ResumeActivityItem");

        Object tx = ctxCls.getMethod("obtain", iatCls, IBinder.class).invoke(null, client, token);
        Object launchItem = obtainItem(launchCls, intent, activityInfo, cfg[0], cfg[1]);
        Object resumeItem = obtainResume(resumeCls);
        // This image's `ClientTransaction` has no public `addTransactionItem`:
        // the items live in the two private fields the executor reads
        // (`mActivityCallbacks`, `mLifecycleStateRequest`), exactly as
        // `addCallback`/`setLifecycleStateRequest` fill them in system_server.
        java.util.ArrayList<Object> callbacks = new java.util.ArrayList<Object>();
        callbacks.add(launchItem);
        setPrivateField(ctxCls, tx, "mActivityCallbacks", callbacks);
        setPrivateField(ctxCls, tx, "mLifecycleStateRequest", resumeItem);
        setPrivateFieldIfPresent(ctxCls, tx, "mTransactionItems", callbacks);
        System.out.println("ACOSLAUNCH MARK transaction-items launch=" + launchItem.getClass().getName()
                + " resume=" + resumeItem.getClass().getName());
        return tx;
    }

    /** The `ClientTransaction` that carries only a lifecycle-state request. */
    static Object lifecycleTransaction(IBinder token, Object lifecycleItem) throws Throwable {
        Class<?> ctxCls = Class.forName("android.app.servertransaction.ClientTransaction");
        Class<?> iatCls = Class.forName("android.app.IApplicationThread");
        Object tx = ctxCls.getMethod("obtain", iatCls, IBinder.class).invoke(null, sClient, token);
        setPrivateField(ctxCls, tx, "mActivityCallbacks", new java.util.ArrayList<Object>());
        setPrivateField(ctxCls, tx, "mLifecycleStateRequest", lifecycleItem);
        setPrivateFieldIfPresent(ctxCls, tx, "mTransactionItems", new java.util.ArrayList<Object>());
        return tx;
    }

    /**
     * Call one lifecycle item's own `obtain(...)` for this image's parameter
     * list: booleans in order from `booleans`, ints `0`, everything else null
     * (the same runtime-shaped matching `obtainItem` uses). The shape is logged
     * so a future image's change is a marker, not a silent wrong transition.
     */
    static Object obtainPrimitiveItem(Class<?> cls, boolean[] booleans) throws Throwable {
        for (java.lang.reflect.Method m : cls.getDeclaredMethods()) {
            if (!m.getName().equals("obtain")) continue;
            Class<?>[] types = m.getParameterTypes();
            boolean primitivesOnly = true;
            for (Class<?> t : types) {
                if (t != boolean.class && t != int.class) primitivesOnly = false;
            }
            if (!primitivesOnly) continue;
            m.setAccessible(true);
            Object[] args = new Object[types.length];
            int bi = 0;
            StringBuilder shape = new StringBuilder();
            for (int i = 0; i < types.length; i++) {
                if (types[i] == boolean.class) {
                    args[i] = booleans[Math.min(bi++, booleans.length - 1)];
                } else {
                    args[i] = 0;
                }
                shape.append(' ').append(i).append(':').append(types[i].getSimpleName());
            }
            System.out.println("ACOSLAUNCH MARK obtain-shape " + cls.getSimpleName() + shape);
            try {
                return m.invoke(null, args);
            } catch (java.lang.reflect.InvocationTargetException e) {
                throw e.getCause() != null ? e.getCause() : e;
            }
        }
        throw new NoSuchMethodException(cls.getName() + ".obtain(primitives)");
    }

    /**
     * W38: start the Activity the app asked for through the stock path.
     *
     * <p>The request has already been recorded host-side by
     * `IActivityTaskManager.startActivity`; this delivers what AMS delivers next:
     * pause the current Activity (its own `ClientTransaction`, because a
     * transaction carries one token), then a `LaunchActivityItem` +
     * `ResumeActivityItem` for a **new real `Binder` token**. Nothing is
     * fabricated when the component is unknown — the caller keeps the
     * structured unsupported path.
     */
    static boolean launchLocalActivity(android.content.Intent intent) throws Throwable {
        android.content.ComponentName cn = intent.getComponent();
        if (cn == null) return false;
        String activity = cn.getClassName();
        String pkg = cn.getPackageName();
        if (!sPkg.equals(pkg)) return false;

        Class<?> ctxCls = Class.forName("android.app.servertransaction.ClientTransaction");
        Class<?> pauseCls = Class.forName("android.app.servertransaction.PauseActivityItem");
        IBinder previous = sCurrentToken;
        if (previous != null) {
            Object pauseItem = obtainPrimitiveItem(pauseCls, new boolean[]{false, false, false, false});
            scheduleLocal(lifecycleTransaction(previous, pauseItem));
            System.out.println("ACOSLAUNCH MARK activity-paused-for-launch token=binder:"
                    + Integer.toHexString(System.identityHashCode(previous)) + " activity=" + activity);
        }
        IBinder token = new android.os.Binder();
        ApplicationInfo appInfo = buildAppInfo();
        Object activityInfo = buildActivityInfoNamed(appInfo, activity);
        Configuration[] cfg = currentAndOverrideConfig(null);
        Object tx = buildLaunchTransactionForIntent(sClient, token, null, intent, activityInfo, cfg);
        scheduleLocal(tx);
        sCurrentToken = token;
        if (previous != null) {
            startStopWatcher(token, previous);
            startFinishWatcher(token, previous);
        }
        System.out.println("ACOSLAUNCH MARK start-activity-launched component=" + pkg + "/" + activity
                + " token=binder:" + Integer.toHexString(System.identityHashCode(token))
                + " previous=binder:" + (previous == null ? "null"
                        : Integer.toHexString(System.identityHashCode(previous))));
        System.out.flush();
        return true;
    }

    /** The live `Activity` object for a token, from `ActivityThread.mActivities`. */
    static Object activityForToken(IBinder token) {
        Object activities = get(sThread, "mActivities");
        if (!(activities instanceof java.util.Map)) return null;
        Object record = ((java.util.Map<?, ?>) activities).get(token);
        return record == null ? null : get(record, "activity");
    }

    /**
     * W38: stop the Activity the new one replaced, once the new one is up.
     *
     * <p>Stock AMS pauses the top before the launch and sends
     * `StopActivityItem(showWindow=false)` for it when the new Activity is
     * visible. The launch transaction only reaches resume, so this waits for the
     * second Activity to be resumed, lets its first traversal run, and then
     * delivers the stop transaction for the old token — a real `Main.onStop`,
     * not a fabricated one. (Visibility is a frame/traversal here; stock uses
     * `ActivityRecord.nowVisible`.)
     */
    static void startStopWatcher(final IBinder token, final IBinder previous) {
        Thread t = new Thread(new Runnable() {
            @Override
            public void run() {
                try {
                    long deadline = System.currentTimeMillis() + 10000;
                    Object activity = null;
                    while (System.currentTimeMillis() < deadline) {
                        activity = activityForToken(token);
                        if (activity != null && Boolean.TRUE.equals(get(activity, "mResumed"))) break;
                        Thread.sleep(10);
                    }
                    if (activity == null) {
                        System.out.println("ACOSLAUNCH MARK stop-watcher-no-activity");
                        return;
                    }
                    // Let `handleResumeActivity` and the first traversal finish.
                    Thread.sleep(200);
                    Class<?> stopCls = Class.forName("android.app.servertransaction.StopActivityItem");
                    Object stopItem = obtainPrimitiveItem(stopCls, new boolean[]{false});
                    scheduleLocal(lifecycleTransaction(previous, stopItem));
                    System.out.println("ACOSLAUNCH MARK activity-stopped-after-launch token=binder:"
                            + Integer.toHexString(System.identityHashCode(previous)));
                    System.out.flush();
                } catch (Throwable x) {
                    System.out.println("ACOSLAUNCH MARK stop-watcher-error " + x);
                    x.printStackTrace(System.out);
                }
            }
        }, "acos-stop-watcher");
        t.setDaemon(true);
        t.start();
    }

    /**
     * ROT6: deliver the lifecycle of a **finished top Activity**.
     *
     * <p>`Activity.finish()` only marks the record (`mFinished`) and tells the
     * server (`IActivityClientController.finishActivity`). Everything an app
     * observes after that — `onPause`, `onSaveInstanceState`/`onStop`,
     * `onDestroy` — is a `ClientTransaction` the *server* sends next. ACOS's
     * ATMS records the finish but has no transaction scheduler for the ART
     * guest, so a plain `back` on a single-Activity app marked the record and
     * then did nothing: the guest acked the key as handled (`Activity.onKeyDown`
     * tracks BACK, `onKeyUp` calls `onBackPressed`), and no lifecycle ran and no
     * frame changed.
     *
     * <p>This is the same shape W38's two-Activity path uses for the Activity it
     * replaced, extended to the three items stock sends for the last one:
     * `PauseActivityItem(finished=true)`, `StopActivityItem(showWindow=false)`,
     * `DestroyActivityItem(finished=true)` — each in its own transaction (a
     * transaction carries one token and one lifecycle state), executed by the
     * framework's own `TransactionExecutor` on the main thread, which is what
     * runs the real `ActivityThread.handlePauseActivity` /
     * `handleStopActivity` / `handleDestroyActivity` and therefore the app's own
     * callbacks. Nothing is fabricated: the items are the framework's, and the
     * trigger is the framework's own `mFinished` flag.
     */
    static void startTopFinishWatcher(final IBinder token) {
        Thread t = new Thread(new Runnable() {
            @Override
            public void run() {
                try {
                    long deadline = System.currentTimeMillis() + 120000;
                    android.util.Log.i("AcosRot6", "ACOSLAUNCH MARK top-finish-watcher-started");
                    Object activity = null;
                    while (System.currentTimeMillis() < deadline) {
                        activity = activityForToken(token);
                        if (activity != null && Boolean.TRUE.equals(get(activity, "mFinished"))) break;
                        Thread.sleep(10);
                    }
                    if (activity == null || !Boolean.TRUE.equals(get(activity, "mFinished"))) {
                        android.util.Log.i("AcosRot6", "ACOSLAUNCH MARK top-finish-watcher-timeout");
                                                return;
                    }
                    Class<?> pauseCls = Class.forName("android.app.servertransaction.PauseActivityItem");
                    Class<?> stopCls = Class.forName("android.app.servertransaction.StopActivityItem");
                    Class<?> destroyCls =
                            Class.forName("android.app.servertransaction.DestroyActivityItem");
                    // PauseActivityItem.obtain(finished, userLeaving, configChanges, dontReport)
                    scheduleLocal(lifecycleTransaction(token,
                            obtainPrimitiveItem(pauseCls, new boolean[]{true, false, false})));
                    scheduleLocal(lifecycleTransaction(token,
                            obtainPrimitiveItem(stopCls, new boolean[]{false})));
                    scheduleLocal(lifecycleTransaction(token,
                            obtainPrimitiveItem(destroyCls, new boolean[]{true, false, false})));
                    android.util.Log.i("AcosRot6", "ACOSLAUNCH MARK activity-destroyed-after-finish token=binder:"
                            + Integer.toHexString(System.identityHashCode(token))
                            + " order=Pause(finished)->Stop->Destroy");
                                    } catch (Throwable x) {
                    android.util.Log.i("AcosRot6", "ACOSLAUNCH MARK top-finish-watcher-error " + x);
                    x.printStackTrace(System.out);
                                    }
            }
        }, "acos-top-finish-watcher");
        t.setDaemon(true);
        t.start();
    }

    /**
     * W38: watch the Activity just launched and, when it has finished
     * (`Activity.finish()` → `IActivityClientController.finishActivity` succeeded
     * host-side), send the two transactions AMS sends next: `DestroyActivityItem`
     * for it, then `ResumeActivityItem` for the Activity below it.
     */
    static void startFinishWatcher(final IBinder token, final IBinder previous) {
        Thread t = new Thread(new Runnable() {
            @Override
            public void run() {
                try {
                    long deadline = System.currentTimeMillis() + 60000;
                    Object activity = null;
                    while (System.currentTimeMillis() < deadline) {
                        activity = activityForToken(token);
                        if (activity != null && Boolean.TRUE.equals(get(activity, "mFinished"))) break;
                        Thread.sleep(10);
                    }
                    if (activity == null || !Boolean.TRUE.equals(get(activity, "mFinished"))) {
                        System.out.println("ACOSLAUNCH MARK finish-watcher-timeout");
                        return;
                    }
                    // ROT8: the order stock ATMS uses when a non-last Activity
                    // finishes (ActivityTaskSupervisor / ActivityRecord.
                    // completePause + resumeTopActivity + activityStopped):
                    //   B.onPause -> A.onStart/onResume (A is redrawn)
                    //   -> B.onStop -> B.onDestroy
                    // The previous Activity is resumed BEFORE the finished one is
                    // stopped/destroyed, so the user never sees an empty task and
                    // A's window is relaid out and drawn while B is still alive.
                    Class<?> pauseCls = Class.forName("android.app.servertransaction.PauseActivityItem");
                    Class<?> stopCls = Class.forName("android.app.servertransaction.StopActivityItem");
                    Class<?> destroyCls = Class.forName("android.app.servertransaction.DestroyActivityItem");
                    Class<?> resumeCls = Class.forName("android.app.servertransaction.ResumeActivityItem");
                    scheduleLocal(lifecycleTransaction(token,
                            obtainPrimitiveItem(pauseCls, new boolean[]{true, false, false})));
                    long pdl = System.currentTimeMillis() + 5000;
                    while (System.currentTimeMillis() < pdl
                            && Boolean.TRUE.equals(get(activity, "mResumed"))) {
                        Thread.sleep(5);
                    }
                    scheduleLocal(lifecycleTransaction(previous, obtainResume(resumeCls)));
                    sCurrentToken = previous;
                    System.out.println("ACOSLAUNCH MARK activity-resumed-after-finish token=binder:"
                            + Integer.toHexString(System.identityHashCode(previous)));
                    long rdl = System.currentTimeMillis() + 10000;
                    Object below = null;
                    while (System.currentTimeMillis() < rdl) {
                        below = activityForToken(previous);
                        if (below != null && Boolean.TRUE.equals(get(below, "mResumed"))) break;
                        Thread.sleep(5);
                    }
                    scheduleLocal(lifecycleTransaction(token,
                            obtainPrimitiveItem(stopCls, new boolean[]{false})));
                    scheduleLocal(lifecycleTransaction(token,
                            obtainPrimitiveItem(destroyCls, new boolean[]{true, false, false})));
                    System.out.println("ACOSLAUNCH MARK activity-destroyed-after-finish token=binder:"
                            + Integer.toHexString(System.identityHashCode(token))
                            + " order=Pause(finished)->Resume(previous)->Stop->Destroy");
                    System.out.flush();
                } catch (Throwable x) {
                    System.out.println("ACOSLAUNCH MARK finish-watcher-error " + x);
                    x.printStackTrace(System.out);
                }
            }
        }, "acos-finish-watcher");
        t.setDaemon(true);
        t.start();
    }

    /**
     * Call the item's own `obtain(...)` with the arguments its **actual**
     * parameter list needs. Values are the ones AMS passes for a normal launch;
     * a parameter type this method does not know is passed as `null` and named
     * in the marker, so a future image's shape change is a log line, not a
     * silent wrong launch.
     */
    static Object obtainItem(Class<?> cls, android.content.Intent intent, Object activityInfo,
            Configuration curConfig, Configuration override) throws Throwable {
        java.lang.reflect.Method obtain = null;
        for (java.lang.reflect.Method m : cls.getDeclaredMethods()) {
            if (!m.getName().equals("obtain")) continue;
            if (obtain == null || m.getParameterCount() > obtain.getParameterCount()) obtain = m;
        }
        if (obtain == null) throw new NoSuchMethodException(cls.getName() + ".obtain");
        obtain.setAccessible(true);
        Class<?>[] types = obtain.getParameterTypes();
        Object[] args = new Object[types.length];
        int ints = 0;
        int configs = 0;
        int booleans = 0;
        StringBuilder shape = new StringBuilder();
        for (int i = 0; i < types.length; i++) {
            Class<?> t = types[i];
            String n = t.getSimpleName();
            Object v = null;
            if (t == IBinder.class) {
                // `Parcel.marshall()` refuses a parcel that carries binder
                // objects ("Tried to marshall a Parcel that contains objects"),
                // and the ACOS transport is a byte pipe, not a binder
                // transaction. Every binder here is a system_server-side token
                // (activity token, assist/shareable/task-fragment tokens) whose
                // identity only matters to AMS; ACOS's AMS stand-in is the peer,
                // so they travel as null. See P4b.md §5.
                v = null;
            } else if (t == android.content.Intent.class) {
                v = intent;
            } else if (t == android.content.pm.ActivityInfo.class) {
                v = activityInfo;
            } else if (t == Configuration.class) {
                v = (configs++ == 0) ? curConfig : override;
            } else if (t == int.class) {
                // ident, deviceId, procState — in the order the record uses them.
                int[] values = {1, 0, 2};
                v = values[Math.min(ints++, values.length - 1)];
            } else if (t == boolean.class) {
                // isForward = true, launchedFromBubble = false.
                v = (booleans++ == 0);
            } else if (t == android.app.ActivityOptions.class) {
                // writeToParcel calls `mActivityOptions.toBundle()`.
                v = android.app.ActivityOptions.makeBasic();
            } else if (t == String.class) {
                v = null;
            }
            args[i] = v;
            shape.append(' ').append(i).append(':').append(n);
        }
        System.out.println("ACOSLAUNCH MARK obtain-shape " + cls.getSimpleName() + shape);
        try {
            return obtain.invoke(null, args);
        } catch (java.lang.reflect.InvocationTargetException e) {
            throw e.getCause() != null ? e.getCause() : e;
        }
    }

    static void setPrivateField(Class<?> cls, Object o, String name, Object value) throws Throwable {
        java.lang.reflect.Field f = cls.getDeclaredField(name);
        f.setAccessible(true);
        f.set(o, value);
    }

    static void setPrivateFieldIfPresent(Class<?> cls, Object o, String name, Object value) {
        try {
            setPrivateField(cls, o, name, value);
        } catch (Throwable ignored) {
        }
    }

    static Object obtainResume(Class<?> cls) throws Throwable {
        for (java.lang.reflect.Method m : cls.getDeclaredMethods()) {
            if (!m.getName().equals("obtain")) continue;
            Class<?>[] t = m.getParameterTypes();
            m.setAccessible(true);
            Object v = null;
            if (t.length == 3 && t[0] == int.class && t[1] == boolean.class && t[2] == boolean.class) {
                v = m.invoke(null, 2, Boolean.TRUE, Boolean.FALSE);
            } else if (t.length == 2 && t[0] == boolean.class && t[1] == boolean.class) {
                v = m.invoke(null, Boolean.TRUE, Boolean.FALSE);
            }
            if (v != null) return v;
        }
        throw new NoSuchMethodException(cls.getName() + ".obtain(boolean,boolean)");
    }

    /** `static Method` lookup by name and arity, then invoke (framework `@hide`). */
    static Object callStatic(Class<?> cls, String name, Object... args) throws Throwable {
        for (java.lang.reflect.Method m : cls.getDeclaredMethods()) {
            if (!m.getName().equals(name)) continue;
            if (m.getParameterCount() != args.length) continue;
            m.setAccessible(true);
            try {
                return m.invoke(null, args);
            } catch (java.lang.reflect.InvocationTargetException e) {
                throw e.getCause() != null ? e.getCause() : e;
            }
        }
        throw new NoSuchMethodException(cls.getName() + "." + name + "/" + args.length);
    }

    /** Field write that fails loudly (an unset ActivityInfo field is a wrong launch). */
    static void setFieldForce(Class<?> cls, Object o, String name, Object value) {
        for (Class<?> c = cls; c != null && c != Object.class; c = c.getSuperclass()) {
            try {
                java.lang.reflect.Field f = c.getDeclaredField(name);
                f.setAccessible(true);
                f.set(o, value);
                return;
            } catch (NoSuchFieldException ignored) {
            } catch (Throwable t) {
                System.out.println("ACOSLAUNCH MARK activityinfo-field " + name + " -> " + t);
                return;
            }
        }
        System.out.println("ACOSLAUNCH MARK activityinfo-field " + name + " -> missing");
    }

    static void setField(Class<?> cls, Object o, String name, Object value) {
        try {
            java.lang.reflect.Field f = cls.getField(name);
            if (value instanceof Integer) f.setInt(o, (Integer) value);
            else if (value instanceof Float) f.setFloat(o, (Float) value);
            else f.set(o, value);
        } catch (Throwable t) {
            System.out.println("ACOSLAUNCH MARK displayinfo-field " + name + " -> " + t);
        }
    }

    /** Hand a marshalled parcel to the peer-owned `acos.launch` binder. */
    static void pushToPeer(int code, byte[] bytes) throws Throwable {
        Class<?> sm = Class.forName("android.os.ServiceManager");
        Method get = sm.getDeclaredMethod("getService", String.class);
        get.setAccessible(true);
        IBinder launch = (IBinder) get.invoke(null, "acos.launch");
        if (launch == null) throw new IllegalStateException("acos.launch binder is null");
        Parcel data = Parcel.obtain();
        Parcel reply = Parcel.obtain();
        try {
            data.writeInterfaceToken("dev.acos.art.ILaunch");
            data.writeByteArray(bytes);
            if (!launch.transact(code, data, reply, 0)) {
                throw new IllegalStateException("acos.launch transact " + code + " returned false");
            }
            reply.readException();
        } finally {
            data.recycle();
            reply.recycle();
        }
    }

    /**
     * Register a reply parcel the peer will replay for `(descriptor, code)`.
     * Used where the host's service codec is not API-34-exact yet; the parcel is
     * produced by the framework class that owns the `writeToParcel`.
     */
    static void pushReplyTemplate(String descriptor, int code, byte[] replyBytes)
            throws Throwable {
        Class<?> sm = Class.forName("android.os.ServiceManager");
        Method get = sm.getDeclaredMethod("getService", String.class);
        get.setAccessible(true);
        IBinder launch = (IBinder) get.invoke(null, "acos.launch");
        if (launch == null) throw new IllegalStateException("acos.launch binder is null");
        Parcel data = Parcel.obtain();
        Parcel reply = Parcel.obtain();
        try {
            // The interface token carries the target descriptor, so the peer
            // reads it with the same string16 reader it uses everywhere else.
            data.writeInterfaceToken(descriptor);
            data.writeInt(code);
            data.writeByteArray(replyBytes);
            if (!launch.transact(2, data, reply, 0)) {
                throw new IllegalStateException("acos.launch template transact returned false");
            }
            reply.readException();
        } finally {
            data.recycle();
            reply.recycle();
        }
    }

    // ---------------------------------------------- scripted activity results

    /**
     * MPROC1: watch for a scripted activity result and deliver it.
     *
     * <p>A session with no installed picker still has to be able to test what
     * the app does with the picture a picker returns. The capture target records
     * the intent; the agent scripts the answer (`resultCode` + a data `Intent`,
     * e.g. an image `content://` URI); this thread turns it into the stock
     * `ActivityResultItem` `ClientTransaction` and hands it to the real
     * `ActivityThread` — `onActivityResult` runs as it would on a device. The
     * file is inside this run's own tree; a JSON parse error is a marker, never
     * a silent drop.
     */
    static void startResultWatcher() {
        Thread t = new Thread(new Runnable() {
            @Override
            public void run() {
                java.io.File file = new java.io.File(sOut, "activity-result.json");
                java.io.File ack = new java.io.File(sOut, "activity-result.ack");
                long lastSeq = -1;
                while (true) {
                    try {
                        Thread.sleep(50);
                        if (!file.isFile()) continue;
                        String text = readText(file.getPath());
                        if (text.isEmpty()) continue;
                        org.json.JSONObject o = new org.json.JSONObject(text);
                        long seq = o.optLong("capture_seq", -1);
                        if (seq <= 0 || seq == lastSeq) continue;
                        int requestCode = o.optInt("request_code", 0);
                        int resultCode = o.optInt("result_code", 0);
                        android.content.Intent data = null;
                        if (o.has("data") || o.has("action") || o.has("mime_type")) {
                            data = new android.content.Intent();
                            if (!o.isNull("action")) data.setAction(o.optString("action"));
                            // `setData` and `setType` each clear the other on
                            // Android; a picker answer carries both, so they go
                            // in through `setDataAndType` (the framework's own
                            // API for exactly this).
                            if (!o.isNull("data") && !o.isNull("mime_type")) {
                                data.setDataAndType(
                                        android.net.Uri.parse(o.optString("data")),
                                        o.optString("mime_type"));
                            } else if (!o.isNull("data")) {
                                data.setData(android.net.Uri.parse(o.optString("data")));
                            } else if (!o.isNull("mime_type")) {
                                data.setType(o.optString("mime_type"));
                            }
                            org.json.JSONObject extras = o.optJSONObject("extras");
                            if (extras != null) {
                                java.util.Iterator<String> keys = extras.keys();
                                while (keys.hasNext()) {
                                    String key = keys.next();
                                    data.putExtra(key, extras.optString(key));
                                }
                            }
                        }
                        deliverScriptedResult(requestCode, resultCode, data);
                        lastSeq = seq;
                        write(ack, "{\"capture_seq\":" + seq
                                + ",\"utc_ms\":" + System.currentTimeMillis() + "}");
                        System.out.println("ACOSLAUNCH MARK activity-result-delivered seq=" + seq
                                + " request_code=" + requestCode + " result_code=" + resultCode
                                + " data=" + (data == null ? "null"
                                        : String.valueOf(data.getDataString())));
                        System.out.flush();
                    } catch (Throwable e) {
                        System.out.println("ACOSLAUNCH MARK activity-result-delivery-error "
                                + unwrap(e));
                        System.out.flush();
                        try {
                            Thread.sleep(200);
                        } catch (InterruptedException ignored) {
                        }
                    }
                }
            }
        });
        t.setDaemon(true);
        t.setName("acos-result-watcher");
        t.start();
        System.out.println("ACOSLAUNCH MARK activity-result-watcher started");
        System.out.flush();
    }

    /**
     * Deliver one `ActivityResultItem` for the current Activity through the real
     * `IApplicationThread.scheduleTransaction` (the same path AMS uses).
     */
    static void deliverScriptedResult(int requestCode, int resultCode,
            android.content.Intent data) throws Throwable {
        IBinder token = sCurrentToken;
        if (token == null) throw new IllegalStateException("no current activity token");
        // `ResultInfo` has no public constructor on this image (only a `Parcel`
        // one, which is package-private for AMS). Build it the way the wire does:
        // write the fields with the framework's own writers
        // (`ResultInfo.writeToParcel` = String16 who, requestCode, resultCode,
        // data presence + `Intent.writeToParcel`) and let `CREATOR` read them
        // back. Nothing here is a hand-marshalled struct.
        Class<?> riCls = Class.forName("android.app.ResultInfo");
        android.os.Parcel rp = android.os.Parcel.obtain();
        Object resultInfo;
        try {
            rp.writeString(null);
            rp.writeInt(requestCode);
            rp.writeInt(resultCode);
            if (data != null) {
                rp.writeInt(1);
                data.writeToParcel(rp, 0);
            } else {
                rp.writeInt(0);
            }
            rp.setDataPosition(0);
            java.lang.reflect.Field creator = riCls.getField("CREATOR");
            Object c = creator.get(null);
            resultInfo = ((android.os.Parcelable.Creator<?>) c).createFromParcel(rp);
        } finally {
            rp.recycle();
        }
        java.util.ArrayList<Object> list = new java.util.ArrayList<Object>();
        list.add(resultInfo);
        Class<?> itemCls =
                Class.forName("android.app.servertransaction.ActivityResultItem");
        Object item = null;
        for (java.lang.reflect.Method m : itemCls.getDeclaredMethods()) {
            if (!m.getName().equals("obtain")) continue;
            Class<?>[] types = m.getParameterTypes();
            if (types.length != 1 || !java.util.List.class.isAssignableFrom(types[0])) continue;
            m.setAccessible(true);
            item = m.invoke(null, list);
        }
        if (item == null) {
            throw new NoSuchMethodException("ActivityResultItem.obtain(List)");
        }
        Class<?> ctxCls = Class.forName("android.app.servertransaction.ClientTransaction");
        Class<?> iatCls = Class.forName("android.app.IApplicationThread");
        Object tx = ctxCls.getMethod("obtain", iatCls, IBinder.class)
                .invoke(null, sClient, token);
        java.util.ArrayList<Object> callbacks = new java.util.ArrayList<Object>();
        callbacks.add(item);
        setPrivateField(ctxCls, tx, "mActivityCallbacks", callbacks);
        setPrivateField(ctxCls, tx, "mLifecycleStateRequest", null);
        setPrivateFieldIfPresent(ctxCls, tx, "mTransactionItems", callbacks);
        scheduleLocal(tx);
    }

    // ------------------------------------------------------------- helpers

    // AOSP `ActivityThread.main`'s per-process mainline-module bootstrap.
    //
    // Hidden framework API (not in the SDK jar this guest source compiles
    // against), so it is reached by reflection the same way `AppMain` reaches
    // `ActivityThread`. A failure is fatal: on a device this call is not
    // guarded either, and a process that boots with null mainline managers
    // dies later in a constructor that names neither the manager nor this
    // call (MODA11Y7). The image dex is the authority for both the entry point
    // (`framework/classes.dex` `ActivityThread.main` insn 001c,
    // `invoke-static {}, Landroid/app/ActivityThread;.initializeMainlineModules:()V`)
    // and the list it installs.
    static void initializeMainlineModules() throws Throwable {
        try {
            Class<?> at = Class.forName("android.app.ActivityThread");
            Method init = at.getDeclaredMethod("initializeMainlineModules");
            init.setAccessible(true);
            init.invoke(null);
            System.out.println("ACOSLAUNCH MARK mainline-modules initialized");
        } catch (InvocationTargetException e) {
            throw e.getCause() != null ? e.getCause() : e;
        }
    }

    static void set(Object o, String name, Object value) {
        for (Class<?> c = o.getClass(); c != null; c = c.getSuperclass()) {
            try {
                Field f = c.getDeclaredField(name);
                f.setAccessible(true);
                f.set(o, value);
                return;
            } catch (NoSuchFieldException ignored) {
            } catch (Throwable ignored) {
                return;
            }
        }
    }

    static Throwable unwrap(Throwable t) {
        while (t instanceof InvocationTargetException && t.getCause() != null) t = t.getCause();
        return t;
    }

    static String esc(String s) {
        return s.replace("\\", "\\\\").replace("\"", "\\\"")
                .replace("\n", "\\n").replace("\r", "\\r").replace("\t", "\\t");
    }

    /**
     * R4 evidence: the fatal throwable's own stack plus every cause's class,
     * message and stack. The one-line `message` names only the outermost
     * wrapper ("Unable to create application …"), which hides the real first
     * failing call that the cause carries.
     */
    static String crashDetail(Throwable t) {
        Throwable u = unwrap(t);
        StringBuilder b = new StringBuilder(1024);
        b.append(",\"exception\":\"").append(esc(u.getClass().getName())).append("\"");
        b.append(",\"stack\":").append(stackJson(u, 24));
        b.append(",\"causes\":[");
        Throwable c = u.getCause();
        boolean first = true;
        int guard = 0;
        while (c != null && c != u && guard++ < 8) {
            if (!first) b.append(',');
            first = false;
            b.append("{\"class\":\"").append(esc(c.getClass().getName()))
                    .append("\",\"message\":\"").append(esc(String.valueOf(c.getMessage())))
                    .append("\",\"stack\":").append(stackJson(c, 16)).append('}');
            Throwable next = c.getCause();
            if (next == c) break;
            c = next;
        }
        b.append("]");
        return b.toString();
    }

    static String stackJson(Throwable t, int max) {
        StackTraceElement[] frames = t.getStackTrace();
        StringBuilder b = new StringBuilder(256);
        b.append('[');
        int n = Math.min(frames.length, max);
        for (int i = 0; i < n; i++) {
            if (i > 0) b.append(',');
            b.append('"').append(esc(String.valueOf(frames[i]))).append('"');
        }
        b.append(']');
        return b.toString();
    }

    static void write(File f, String s) throws Exception {
        FileOutputStream out = new FileOutputStream(f);
        try {
            out.write(s.getBytes("UTF-8"));
        } finally {
            out.close();
        }
    }

    /**
     * R3: `ACOSLAUNCH_CRASH_TEST` — deliberately fault the guest once it is up.
     *
     * <p>`segv` = store to an unmapped page on the main thread (SIGSEGV), `abort`
     * = SIGABRT, `thread` = the SIGSEGV on a secondary thread named
     * `acos-crash-test` (proves the faulting thread name is captured, not the
     * main thread's). Unset: nothing happens. Only the launch guest calls this.
     */
    static void crashTestIfRequested(Object thread, Object activity) {
        String mode = System.getenv("ACOSLAUNCH_CRASH_TEST");
        if (mode == null || mode.isEmpty()) return;
        mode = mode.trim();
        System.out.println("ACOSLAUNCH MARK crash-test mode=" + mode);
        System.out.flush();
        if (mode.equals("main")) {
            // The fault on the framework's main thread (the thread a UI bug
            // usually faults on), posted so the looper is the one running it.
            new android.os.Handler(android.os.Looper.getMainLooper()).post(new Runnable() {
                public void run() {
                    System.out.println("ACOSLAUNCH MARK crash-test-main tid="
                            + android.os.Process.myTid());
                    System.out.flush();
                    Nat.crashTest(0);
                }
            });
            return;
        }
        if (mode.equals("thread")) {
            Thread t = new Thread(new Runnable() {
                public void run() {
                    System.out.println("ACOSLAUNCH MARK crash-test-thread tid="
                            + android.os.Process.myTid());
                    System.out.flush();
                    Nat.crashTest(0);
                }
            }, "acos-crash-test");
            t.start();
            try {
                t.join(5000);
            } catch (InterruptedException ignored) {
            }
            return;
        }
        Nat.crashTest(mode.equals("abort") ? 1 : 0);
    }

    /**
     * CAMERA3 Option A: guest-side frame-sink binder registered once per app
     * process under {@code acos.camera.frame_sink}. The host {@code hw.camera}
     * Session Controller drives stream registration, frame delivery, and stream
     * teardown over ordered oneway deliveries while pixels are written directly
     * into the app's own local {@code Surface} ({@code ANativeWindow}).
     */
    private static final String CAMERA_SINK_SERVICE = "acos.camera.frame_sink";
    private static final String CAMERA_SINK_DESCRIPTOR = "dev.acos.camera.ICameraFrameSink";
    private static final int CAMERA_SINK_OP_REGISTER = 1;
    private static final int CAMERA_SINK_OP_DELIVER = 2;
    private static final int CAMERA_SINK_OP_RELEASE = 3;
    private static final int CAMERA_SINK_OP_DELIVER_FD = 4;
    private static android.os.Binder sCameraFrameSink;
    private static final java.util.HashMap<Integer, android.view.Surface> sCameraSinkSurfaces =
            new java.util.HashMap<Integer, android.view.Surface>();

    static void registerCameraFrameSink() {
        try {
            sCameraFrameSink = new android.os.Binder() {
                @Override
                protected boolean onTransact(int code, Parcel data, Parcel reply, int flags)
                        throws android.os.RemoteException {
                    if (code == CAMERA_SINK_OP_REGISTER) {
                        data.enforceInterface(CAMERA_SINK_DESCRIPTOR);
                        int streamId = data.readInt();
                        android.view.Surface surface =
                                android.view.Surface.CREATOR.createFromParcel(data);
                        int width = data.readInt();
                        int height = data.readInt();
                        int rc = RenderNat.cameraSinkRegister(streamId, surface, width, height);
                        if (rc != 0) {
                            System.out.println("ACOSLAUNCH WARN camera-sink register stream="
                                    + streamId + " rc=" + rc);
                        }
                        if (surface != null) {
                            synchronized (sCameraSinkSurfaces) {
                                android.view.Surface prev =
                                        sCameraSinkSurfaces.put(streamId, surface);
                                if (prev != null) prev.release();
                            }
                        }
                        return true;
                    }
                    if (code == CAMERA_SINK_OP_DELIVER) {
                        data.enforceInterface(CAMERA_SINK_DESCRIPTOR);
                        int streamId = data.readInt();
                        int width = data.readInt();
                        int height = data.readInt();
                        long frameNumber = data.readLong();
                        long timestampNs = data.readLong();
                        int patternId = data.readInt();
                        int rc = RenderNat.cameraSinkDeliver(
                                streamId, width, height, frameNumber, timestampNs, patternId);
                        if (rc != 0) {
                            System.out.println("ACOSLAUNCH WARN camera-sink deliver stream="
                                    + streamId + " frame=" + frameNumber + " rc=" + rc);
                        }
                        return true;
                    }
                    if (code == CAMERA_SINK_OP_DELIVER_FD) {
                        data.enforceInterface(CAMERA_SINK_DESCRIPTOR);
                        int streamId = data.readInt();
                        int width = data.readInt();
                        int height = data.readInt();
                        long frameNumber = data.readLong();
                        long timestampNs = data.readLong();
                        int patternId = data.readInt();
                        android.os.ParcelFileDescriptor pfd = data.readFileDescriptor();
                        int rc = -1;
                        try {
                            int fd = pfd != null ? pfd.getFd() : -1;
                            rc = RenderNat.cameraSinkDeliverFd(
                                    streamId, width, height, frameNumber, timestampNs, patternId, fd);
                        } finally {
                            if (pfd != null) {
                                try {
                                    pfd.close();
                                } catch (Throwable ignored) {
                                }
                            }
                        }
                        if (rc != 0) {
                            System.out.println("ACOSLAUNCH WARN camera-sink deliverFd stream="
                                    + streamId + " frame=" + frameNumber + " rc=" + rc);
                        }
                        return true;
                    }
                    if (code == CAMERA_SINK_OP_RELEASE) {
                        data.enforceInterface(CAMERA_SINK_DESCRIPTOR);
                        int streamId = data.readInt();
                        RenderNat.cameraSinkRelease(streamId);
                        synchronized (sCameraSinkSurfaces) {
                            if (streamId < 0) {
                                for (android.view.Surface s : sCameraSinkSurfaces.values()) {
                                    if (s != null) s.release();
                                }
                                sCameraSinkSurfaces.clear();
                            } else {
                                android.view.Surface s = sCameraSinkSurfaces.remove(streamId);
                                if (s != null) s.release();
                            }
                        }
                        return true;
                    }
                    return super.onTransact(code, data, reply, flags);
                }
            };
            Class<?> sm = Class.forName("android.os.ServiceManager");
            Method add = sm.getMethod("addService", String.class, IBinder.class);
            add.invoke(null, CAMERA_SINK_SERVICE, sCameraFrameSink);
        } catch (Throwable t) {
            System.out.println("ACOSLAUNCH WARN camera-sink register failed: " + t);
        }
    }
}
