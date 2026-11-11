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
 *       {@code ActivityThread} then runs {@code Activity.onCreate/onResume}.</li>
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
    static volatile boolean sApplicationCreated;
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
        try {
            run(t0);
        } catch (Throwable t) {
            Throwable u = unwrap(t);
            u.printStackTrace(System.out);
            System.out.println("ACOSLAUNCH {\"outcome\":\"crash\",\"code\":\"GUEST_ERROR\",\"message\":\""
                    + esc(String.valueOf(u)) + "\"}");
            System.out.flush();
            System.exit(1);
        }
    }

    static void run(long t0) throws Throwable {
        registerNatives();
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
        int looper = Nat.startBinderLooper();
        if (looper != 1) throw new IllegalStateException("startBinderLooper -> " + looper);
        System.out.println("ACOSLAUNCH MARK binder-looper started");

        Class<?> at = Class.forName("android.app.ActivityThread");
        Constructor<?> ctor = at.getDeclaredConstructor();
        ctor.setAccessible(true);
        Object thread = ctor.newInstance();
        Field fAppThread = at.getDeclaredField("mAppThread");
        fAppThread.setAccessible(true);
        IBinder appThread = (IBinder) fAppThread.get(thread);
        if (appThread == null) throw new IllegalStateException("ActivityThread.mAppThread == null");

        byte[] bind = buildBindApplicationRequest();
        pushToPeer(1, bind);
        System.out.println("ACOSLAUNCH MARK bind-request-pushed bytes=" + bind.length);
        byte[] display = buildDisplayInfoReply();
        pushReplyTemplate("android.hardware.display.IDisplayManager", 1, display);
        System.out.println("ACOSLAUNCH MARK displayinfo-reply-pushed bytes=" + display.length);
        byte[] appInfo = buildApplicationInfoReply();
        pushReplyTemplate("android.content.pm.IPackageManager", 9, appInfo);
        System.out.println("ACOSLAUNCH MARK applicationinfo-reply-pushed bytes=" + appInfo.length);
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

        // The bindApplication H message is already posted; run the real main
        // looper. The watcher acts as the AMS side of the protocol: it waits for
        // the real `Application` to exist, then (for activity-resumed) sends the
        // one ClientTransaction AMS would send.
        startWatcher(thread, at, appThread);
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
                    + esc(String.valueOf(unwrap(fatal))) + "\"}");
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
                    + " layout_requested=" + get(vr, "mLayoutRequested"));
        } catch (Throwable x) {
            System.out.println("ACOSLAUNCH MARK viewroot-probe failed " + x);
        }
    }

    static volatile boolean sActivityResumed;
    /** The framework printed `<<<<< Finished` for the BIND_APPLICATION message. */
    static volatile boolean sBindFinished;

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
     * activity, and waits for the real {@code Activity.mResumed}.
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
                + "\nsourceDir=" + sApk + "\n";
        write(new File(sOut, "application-created.marker"), marker);
        System.out.println("ACOSLAUNCH MARK application-created package=" + sPkg
                + " application=" + app.getClass().getName()
                + " base=" + appBase
                + " instrumentation=" + (instr == null ? "null" : instr.getClass().getName()));
        System.out.flush();

        if (!"activity-resumed".equals(sUntil) || sActivity == null) {
            // Wait for the framework's own `<<<<< Finished` line for the
            // BIND_APPLICATION message: that is the proof the real
            // `handleBindApplication` (and therefore
            // `Instrumentation.callApplicationOnCreate`) returned normally.
            long bindDeadline = System.currentTimeMillis() + 5000;
            while (!sBindFinished && System.currentTimeMillis() < bindDeadline) Thread.sleep(5);
            System.out.println("ACOSLAUNCH MARK bind-application-dispatch-finished=" + sBindFinished);
            awaitFrameProbe();
            quit();
            return;
        }
        // ---- the AMS step: one ClientTransaction to the app thread ---------
        awaitFrameProbe();
        IBinder token = new android.os.Binder();
        try {
            byte[] txn = buildScheduleTransactionRequest(appThread, token, app);
            pushToPeer(3, txn);
            System.out.println("ACOSLAUNCH MARK schedule-transaction-pushed bytes=" + txn.length
                    + " activity=" + sActivity + " token=local-binder");
        } catch (Throwable x) {
            System.out.println("ACOSLAUNCH MARK schedule-transaction-failed " + x);
            x.printStackTrace(System.out);
            dumpActivityState(thread, at);
            quit();
            return;
        }
        System.out.flush();
        long adeadline = System.currentTimeMillis() + 12000;
        while (System.currentTimeMillis() < adeadline) {
            Object activity = resumedActivity(thread);
            if (activity != null) {
                // Let the fixture's own onResume() body finish and log.
                Thread.sleep(200);
                sActivityResumed = true;
                writeResumeMarker(thread, activity, null);
                System.out.println("ACOSLAUNCH MARK activity-resumed activity="
                        + activity.getClass().getName() + " window=" + get(activity, "mWindow"));
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
                        Thread.sleep(ms);
                        System.out.println("ACOSLAUNCH MARK held-after-resume ms=" + ms);
                        probeViewRoot(activity);
                    } catch (Throwable ignored) {
                    }
                }
                quit();
                return;
            }
            Thread.sleep(5);
        }
        System.out.println("ACOSLAUNCH MARK watchdog: activity not resumed");
        dumpActivityState(thread, at);
        write(new File(sOut, "activity-not-resumed.marker"),
                "package=" + sPkg + "\nactivity=" + sActivity + "\n");
        quit();
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
    static String writeResumeMarker(Object thread, Object activity, String note) throws Exception {
        String am = "package=" + sPkg + "\nactivity=" + activity.getClass().getName()
                + "\nlifecycle=onCreate,onStart,onResume"
                + "\nresumed=" + get(activity, "mResumed")
                + "\ncalled=" + get(activity, "mCalled")
                + "\nstopped=" + get(activity, "mStopped")
                + "\nwindow=" + get(activity, "mWindow")
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

    static void registerNatives() {
        RenderNat.load();
        String[] names = {
                // ActivityThread's own natives (nInitZygoteChildHeapProfiling is
                // called from handleBindApplication).
                "register_android_app_ActivityThread",
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
                // DisplayManagerGlobal's constructor reads OverlayProperties
                // (IDisplayManager.getOverlaySupport) even when the reply is
                // null; the CREATOR field reference runs <clinit>.
                "register_android_hardware_OverlayProperties",
                "register_com_android_internal_util_VirtualRefBasePtr",
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
        ai.nativeLibraryDir = "/data/app";
        ai.processName = sPkg;
        ai.uid = android.os.Process.myUid();
        ai.minSdkVersion = 26;
        ai.targetSdkVersion = 34;
        // FLAG_HAS_CODE(0x4) | FLAG_INSTALLED(0x800000)
        ai.flags = 0x4 | 0x800000;
        // No `FLAG_HARDWARE_ACCELERATED`: the platform default for an app that
        // does not ask for it is the software ViewRootImpl path, which is the
        // only drawing route a surface-less ACOS window can serve at all
        // (P4b.md §4).
        set(ai, "deviceProtectedDataDir", ai.dataDir);
        set(ai, "credentialProtectedDataDir", ai.dataDir);
        set(ai, "compileSdkVersion", 34);
        set(ai, "appComponentFactory", "android.app.AppComponentFactory");
        set(ai, "enabled", Boolean.TRUE);
        set(ai, "enabledSetting", 0);
        return ai;
    }

    /**
     * The `getApplicationInfo` reply the framework would get from the package
     * manager for the APK being launched (no split: the launch needs no class
     * outside the APK, because `instrumentationName` is null).
     */
    static byte[] buildApplicationInfoReply() throws Throwable {
        Parcel p = Parcel.obtain();
        try {
            p.writeInt(0); // AIDL reply: no exception
            p.writeTypedObject(buildAppInfo(), 0);
            return p.marshall();
        } finally {
            p.recycle();
        }
    }

    static byte[] buildBindApplicationRequest() throws Throwable {
        ApplicationInfo ai = buildAppInfo();

        Object providerList = providerList();

        Configuration cfg = new Configuration();
        cfg.densityDpi = 420;
        cfg.screenWidthDp = 411;
        cfg.screenHeightDp = 842;
        cfg.smallestScreenWidthDp = 411;
        cfg.orientation = Configuration.ORIENTATION_PORTRAIT;
        cfg.fontScale = 1.0f;
        cfg.setLocales(android.os.LocaleList.forLanguageTags("en-US"));

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

    static Object providerList() throws Throwable {
        Class<?> cls = Class.forName("android.content.pm.ProviderInfoList");
        Method fromList = cls.getMethod("fromList", java.util.List.class);
        return fromList.invoke(null, new ArrayList<Object>());
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
                    {"displayId", "0"}, {"logicalWidth", "1080"},
                    {"logicalHeight", "2400"}, {"appWidth", "1080"},
                    {"appHeight", "2400"}, {"name", "Built-in Screen"},
                    {"flags", "0"}, {"type", "0"},
                    {"rotation", "0"}, {"modeId", "0"},
                    {"logicalDensityDpi", "420"}, {"state", "2"},
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
        setField(cls, info, "logicalWidth", 1080);
        setField(cls, info, "logicalHeight", 2400);
        setField(cls, info, "appWidth", 1080);
        setField(cls, info, "appHeight", 2209);
        setField(cls, info, "smallestNominalAppWidth", 1080);
        setField(cls, info, "smallestNominalAppHeight", 2209);
        setField(cls, info, "largestNominalAppWidth", 1080);
        setField(cls, info, "largestNominalAppHeight", 2209);
        setField(cls, info, "name", "Built-in Screen");
        setField(cls, info, "flags", 0);
        setField(cls, info, "type", 1);
        setField(cls, info, "rotation", 0);
        setField(cls, info, "modeId", 1);
        setField(cls, info, "defaultModeId", 1);
        setField(cls, info, "logicalDensityDpi", 420);
        setField(cls, info, "state", 2);
        setField(cls, info, "colorMode", 0);
        setField(cls, info, "installOrientation", 0);
        setField(cls, info, "refreshRate", 60.000004f);
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
        setField(cls, info, "nativeLibraryDir", "/data/app");
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
        Class<?> cls = Class.forName("android.content.pm.ActivityInfo");
        Object ai = cls.getDeclaredConstructor().newInstance();
        setFieldForce(cls, ai, "applicationInfo", appInfo);
        setFieldForce(cls, ai, "name", sActivity);
        setFieldForce(cls, ai, "packageName", sPkg);
        setFieldForce(cls, ai, "taskAffinity", sPkg);
        setFieldForce(cls, ai, "targetActivity", null);
        setFieldForce(cls, ai, "permission", null);
        setFieldForce(cls, ai, "launchMode", 0);
        setFieldForce(cls, ai, "theme", 0);          // 0 = platform default
        setFieldForce(cls, ai, "flags", 0);          // no FLAG_HARDWARE_ACCELERATED
        setFieldForce(cls, ai, "screenOrientation", -1);
        setFieldForce(cls, ai, "configChanges", 0);
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
        ApplicationInfo appInfo = buildAppInfo();
        Object activityInfo = buildActivityInfo(appInfo);

        Configuration base = new Configuration();
        base.densityDpi = 420;
        base.screenWidthDp = 411;
        base.screenHeightDp = 842;
        base.smallestScreenWidthDp = 411;
        base.orientation = Configuration.ORIENTATION_PORTRAIT;
        base.fontScale = 1.0f;
        base.setLocales(android.os.LocaleList.forLanguageTags("en-US"));
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

        android.content.Intent intent = new android.content.Intent(android.content.Intent.ACTION_MAIN);
        intent.addCategory(android.content.Intent.CATEGORY_LAUNCHER);
        intent.setComponent(new ComponentName(sPkg, sActivity));
        intent.setFlags(android.content.Intent.FLAG_ACTIVITY_NEW_TASK);

        Class<?> ctxCls = Class.forName("android.app.servertransaction.ClientTransaction");
        Class<?> itemCls = Class.forName("android.app.servertransaction.ClientTransactionItem");
        Class<?> iatCls = Class.forName("android.app.IApplicationThread");
        Class<?> iatStub = Class.forName("android.app.IApplicationThread$Stub");
        Class<?> launchCls = Class.forName("android.app.servertransaction.LaunchActivityItem");
        Class<?> resumeCls = Class.forName("android.app.servertransaction.ResumeActivityItem");

        Object client = iatStub.getMethod("asInterface", IBinder.class).invoke(null, appThread);
        Object tx = ctxCls.getMethod("obtain", iatCls, IBinder.class).invoke(null, client, null);

        Object launchItem = obtainItem(launchCls, intent, activityInfo, curConfig, override);
        Object resumeItem = obtainResume(resumeCls);
        // This image's `ClientTransaction` has no public `addTransactionItem`:
        // the items live in the two private fields the executor reads
        // (`mActivityCallbacks`, `mLifecycleStateRequest`), exactly as
        // `addCallback`/`setLifecycleStateRequest` fill them in system_server.
        java.util.ArrayList<Object> callbacks = new java.util.ArrayList<Object>();
        callbacks.add(launchItem);
        setPrivateField(ctxCls, tx, "mActivityCallbacks", callbacks);
        setPrivateField(ctxCls, tx, "mLifecycleStateRequest", resumeItem);
        // Present on newer images (the bundled-item shape); a no-op here.
        setPrivateFieldIfPresent(ctxCls, tx, "mTransactionItems", callbacks);
        System.out.println("ACOSLAUNCH MARK transaction-items launch=" + launchItem.getClass().getName()
                + " resume=" + resumeItem.getClass().getName());

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

    // ------------------------------------------------------------- helpers

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
        return s.replace("\\", "\\\\").replace("\"", "\\\"");
    }

    static void write(File f, String s) throws Exception {
        FileOutputStream out = new FileOutputStream(f);
        try {
            out.write(s.getBytes("UTF-8"));
        } finally {
            out.close();
        }
    }
}
