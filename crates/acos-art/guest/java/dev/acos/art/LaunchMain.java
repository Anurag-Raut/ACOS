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
 *       {@code LoadedApk}, the {@code Application} from the APK, and runs
 *       {@code Application.onCreate};</li>
 *   <li>{@link Instr} (the manifest-declared instrumentation) records the
 *       marker and stops the looper. The activity is <b>not</b> launched.
 *   </li>
 * </ol>
 *
 * argv: apk guestPackage outDir
 */
public final class LaunchMain {
    static String sApk;
    static String sPkg;
    static String sOut;
    static volatile boolean sApplicationCreated;

    public static void main(String[] args) {
        long t0 = System.currentTimeMillis();
        if (args.length < 3) {
            System.out.println("ACOSLAUNCH {\"outcome\":\"crash\",\"code\":\"BAD_ARGS\"}");
            System.exit(2);
        }
        sApk = args[0];
        sPkg = args[1];
        sOut = args[2];
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
        writeSplitApk();
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

        // The bindApplication H message is already posted; run the real main
        // looper. Instr stops it after Application.onCreate.
        startWatchdog(thread, at);
        Looper.loop();

        long wall = System.currentTimeMillis() - t0;
        if (!sApplicationCreated) {
            System.out.println("ACOSLAUNCH {\"outcome\":\"crash\",\"code\":\"NO_APPLICATION\"}");
            System.exit(1);
        }
        String result = "{\"outcome\":\"ok\",\"package\":\"" + esc(sPkg) + "\",\"apk\":\""
                + esc(sApk) + "\",\"application_created\":true,\"activity_launched\":false,"
                + "\"wall_ms\":" + wall + "}";
        write(new File(sOut, "launch.json"), result);
        System.out.println("ACOSLAUNCH " + result);
        System.out.flush();
        // Dalvik's main thread has quit its looper; exit explicitly so the
        // process does not wait for non-daemon binder threads.
        System.exit(0);
    }

    /** Diagnostics: if the bind message never completes, say where it stopped. */
    static void startWatchdog(final Object thread, final Class<?> at) {
        Thread t = new Thread(new Runnable() {
            @Override
            public void run() {
                try {
                    Thread.sleep(5000);
                } catch (InterruptedException ignored) {
                }
                if (sApplicationCreated) return;
                System.out.println("ACOSLAUNCH MARK watchdog: application not created");
                for (String f : new String[]{"mBoundApplication", "mInitialApplication",
                        "mInstrumentation", "mSystemThread"}) {
                    try {
                        Field fd = at.getDeclaredField(f);
                        fd.setAccessible(true);
                        System.out.println("ACOSLAUNCH MARK " + f + "=" + fd.get(thread));
                    } catch (Throwable x) {
                        System.out.println("ACOSLAUNCH MARK " + f + " -> " + x);
                    }
                }
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
        }, "acos-launch-watchdog");
        t.setDaemon(true);
        t.start();
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
        // The launch instrumentation class is not in the app's base APK. Android
        // loads it from a split, exactly as a real test APK does; the base APK
        // stays byte-identical. The split is `classes.dex` of this guest, zipped
        // under /data/app at startup.
        ai.splitSourceDirs = new String[]{SPLIT_APK};
        ai.splitNames = new String[]{"acos-launch"};
        set(ai, "deviceProtectedDataDir", ai.dataDir);
        set(ai, "credentialProtectedDataDir", ai.dataDir);
        set(ai, "compileSdkVersion", 34);
        set(ai, "appComponentFactory", "android.app.AppComponentFactory");
        set(ai, "enabled", Boolean.TRUE);
        set(ai, "enabledSetting", 0);
        return ai;
    }

    /**
     * The `getApplicationInfo` reply the framework would get from a package that
     * declares the launch split. `PackageManager` caches the `LoadedApk` built
     * from this reply, and `handleBindApplication` reuses that cache entry, so
     * the split list must be visible here too (otherwise the instrumentation
     * class cannot be loaded — see P4a.md).
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
        ComponentName instrName = new ComponentName(sPkg, "dev.acos.art.LaunchMain$Instr");

        Parcel p = Parcel.obtain();
        try {
            p.writeInterfaceToken("android.app.IApplicationThread");
            p.writeString(sPkg);                                   // packageName
            p.writeTypedObject(ai, 0);                             // ApplicationInfo
            p.writeString(sPkg);                                   // processName
            p.writeString(null);                                   // sdkSandbox volume
            p.writeTypedObject((android.os.Parcelable) providerList, 0); // ProviderInfoList
            p.writeTypedObject(instrName, 0);                      // instrumentationName
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

    static final String GUEST_DEX = "/data/app/launch-guest/classes.dex";
    static final String SPLIT_APK = "/data/app/launch-split.apk";

    /** Zip this guest's dex as a split APK, so the app class loader finds the instrumentation. */
    static void writeSplitApk() {
        java.io.File split = new java.io.File(SPLIT_APK);
        if (split.isFile() && split.length() > 0) return;
        try (java.util.zip.ZipOutputStream z =
                     new java.util.zip.ZipOutputStream(new FileOutputStream(split))) {
            z.putNextEntry(new java.util.zip.ZipEntry("classes.dex"));
            java.io.FileInputStream in = new java.io.FileInputStream(GUEST_DEX);
            try {
                byte[] buf = new byte[65536];
                int n;
                while ((n = in.read(buf)) > 0) z.write(buf, 0, n);
            } finally {
                in.close();
            }
            z.closeEntry();
        } catch (Throwable t) {
            System.out.println("ACOSLAUNCH MARK split-apk-failed " + t);
        }
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

    // ------------------------------------------------- instrumentation hook

    /**
     * The framework instantiates this by the {@code instrumentationName} in the
     * bind request and calls it on the real path:
     * {@code handleBindApplication -> Instrumentation.callApplicationOnCreate}.
     * It therefore proves {@code Application.onCreate} ran for the APK's
     * Application, not a guest re-implementation.
     */
    public static final class Instr extends Instrumentation {
        @Override
        public void onCreate(Bundle arguments) {
            System.out.println("ACOSLAUNCH MARK instrumentation-onCreate");
        }

        @Override
        public void callApplicationOnCreate(Application app) {
            super.callApplicationOnCreate(app);
            sApplicationCreated = true;
            try {
                File dir = new File(sOut);
                dir.mkdirs();
                write(new File(dir, "application-created.marker"),
                        "package=" + sPkg + "\napplication=" + app.getClass().getName()
                                + "\nsourceDir=" + sApk + "\n");
            } catch (Throwable t) {
                System.out.println("ACOSLAUNCH MARK marker-write-failed " + t);
            }
            System.out.println("ACOSLAUNCH MARK application-created package=" + sPkg
                    + " application=" + app.getClass().getName()
                    + " base=" + app.getBaseContext());
            System.out.flush();
            // Stop the main looper after the bind message returns; the activity
            // is never launched.
            new android.os.Handler(Looper.getMainLooper()).post(new Runnable() {
                @Override
                public void run() {
                    Looper.myLooper().quitSafely();
                }
            });
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
