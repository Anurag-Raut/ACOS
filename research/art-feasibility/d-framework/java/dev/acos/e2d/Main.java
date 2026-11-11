package dev.acos.e2d;

import android.content.res.AssetManager;
import android.content.res.Configuration;
import android.content.res.Resources;
import android.os.Bundle;
import android.os.Handler;
import android.os.Looper;
import android.os.Parcel;
import android.util.DisplayMetrics;

import java.lang.reflect.Method;
import java.util.concurrent.atomic.AtomicInteger;

/**
 * E2D: run Android *framework* Java classes on real ART, no Android boot.
 *
 * One subcommand per rung of the ladder so that a blocker on rung N does not
 * hide rung N+1.  Every rung prints PASS/FAIL plus the first blocker text.
 *
 * Compiled against the SDK stubs (android.jar) because the device
 * framework.jar is dex-only; the @hide surfaces (BinderInternal,
 * ServiceManager, ActivityThread, AssetManager.addAssetPath) are reached by
 * reflection -- there is no hidden-API policy here, we are not an app.
 *
 *   dalvikvm -cp /data/dex/e2d/classes.dex dev.acos.e2d.Main <step>
 */
public final class Main {

    static int failures = 0;

    public static void main(String[] args) {
        String step = args.length > 0 ? args[0] : "all";
        System.out.println("== E2D step=" + step + " ==");
        try {
            switch (step) {
                case "bundle":    stepBundle(); break;
                case "parcel":    stepParcel(); break;
                case "looper":    stepLooper(); break;
                case "binder":    stepBinder(); break;
                case "resources": stepResources(); break;
                case "activity":  stepActivity(); break;
                default:
                    stepBundle(); stepParcel(); stepLooper(); stepBinder();
                    stepResources(); stepActivity();
            }
        } catch (Throwable t) {
            System.out.println("FATAL " + t);
            t.printStackTrace(System.out);
        }
        System.out.println("== E2D done failures=" + failures + " ==");
        System.exit(failures == 0 ? 0 : 1);
    }

    static void ok(String s) { System.out.println("PASS " + s); }

    static void bad(String s, Throwable t) {
        failures++;
        System.out.println("FAIL " + s + " : " + t);
        StackTraceElement[] st = t.getStackTrace();
        for (int i = 0; i < Math.min(4, st.length); i++) System.out.println("     at " + st[i]);
        Throwable c = t.getCause();
        while (c != null) { System.out.println("     cause: " + c); c = c.getCause(); }
    }

    // ------------------------------------------------------------- reflection
    static Class<?> cls(String n) {
        try {
            return Class.forName(n, true, Main.class.getClassLoader());
        } catch (Throwable t) {
            throw new RuntimeException("Class.forName(" + n + ")", t);
        }
    }

    static Method m(Class<?> c, String name, Class<?>... types) {
        try {
            Method mm = c.getDeclaredMethod(name, types);
            mm.setAccessible(true);
            return mm;
        } catch (Throwable t) {
            throw new RuntimeException(c.getName() + "." + name, t);
        }
    }

    static Object staticCall(String cls, String method, Object... a) {
        Class<?> c = cls(cls);
        Class<?>[] types = new Class<?>[a.length];
        for (int i = 0; i < a.length; i++) types[i] = a[i] == null ? Object.class : a[i].getClass();
        Method mm = null;
        for (Method cand : c.getDeclaredMethods()) {
            if (!cand.getName().equals(method) || cand.getParameterCount() != a.length) continue;
            mm = cand;
            break;
        }
        if (mm == null) mm = m(c, method, types);
        mm.setAccessible(true);
        try {
            return mm.invoke(null, a);
        } catch (java.lang.reflect.InvocationTargetException e) {
            throw new RuntimeException(e.getCause());
        } catch (IllegalAccessException e) {
            throw new RuntimeException(e);
        }
    }

    static Object call(Object target, String method, Object... a) {
        for (Method cand : target.getClass().getDeclaredMethods()) {
            if (!cand.getName().equals(method) || cand.getParameterCount() != a.length) continue;
            cand.setAccessible(true);
            try {
                return cand.invoke(target, a);
            } catch (java.lang.reflect.InvocationTargetException e) {
                throw new RuntimeException(e.getCause());
            } catch (IllegalAccessException e) {
                throw new RuntimeException(e);
            }
        }
        throw new RuntimeException("no " + method + "/" + a.length + " on " + target.getClass());
    }

    // ---------------------------------------------------------------- rung 1
    /** android.os.Bundle is pure Java over the boot classpath: no native code. */
    static void stepBundle() {
        try {
            Bundle b = new Bundle();
            b.putString("k", "v");
            b.putInt("i", 42);
            b.putLong("l", 1L << 40);
            b.putStringArray("a", new String[]{"x", "y"});
            String got = b.getString("k");
            int i = b.getInt("i");
            if (!"v".equals(got) || i != 42) throw new IllegalStateException("bad " + got + "/" + i);
            ok("[1] Bundle (pure Java, no natives): put/get String,int,long,String[] -> "
               + got + "," + i + "," + b.getLong("l") + "," + b.getStringArray("a").length);
        } catch (Throwable t) {
            bad("[1] Bundle", t);
        }
    }

    // ---------------------------------------------------------------- rung 2
    static void stepParcel() {
        // (a) no natives registered yet -- expected UnsatisfiedLinkError
        try {
            Parcel p = Parcel.obtain();
            p.writeString("hello");
            ok("[2a] Parcel.obtain+writeString WITHOUT android_runtime -> size=" + p.dataSize());
            p.recycle();
        } catch (Throwable t) {
            // Expected: this is rung 0 of the ladder, recorded not counted.
            System.out.println("OBSERVED [2a] Parcel without libandroid_runtime: " + t);
        }

        if (!loadRuntime()) return;

        try {
            int rc = Nat.register("register_android_os_Parcel");
            if (rc != 1) throw new IllegalStateException("register rc=" + rc);
            ok("[2c] android::register_android_os_Parcel(env)");
        } catch (Throwable t) {
            bad("[2c] register_android_os_Parcel", t);
            return;
        }

        try {
            Parcel p = Parcel.obtain();
            p.writeString("hello");
            p.writeInt(7);
            Bundle b = new Bundle();
            b.putString("n", "bundle-in-parcel");
            p.writeBundle(b);
            int size = p.dataSize();
            p.setDataPosition(0);
            String s = p.readString();
            int i = p.readInt();
            Bundle b2 = p.readBundle(Main.class.getClassLoader());
            p.recycle();
            if (!"hello".equals(s) || i != 7
                || !"bundle-in-parcel".equals(b2.getString("n"))) {
                throw new IllegalStateException("roundtrip " + s + "/" + i + "/" + b2.getString("n"));
            }
            ok("[2d] Parcel writeString/readString + writeInt + writeBundle/readBundle, "
               + size + " bytes, no binder touched");
        } catch (Throwable t) {
            bad("[2d] Parcel round trip", t);
        }

        try {
            Parcel p = Parcel.obtain();
            p.writeString("marshalled");
            byte[] raw = p.marshall();
            p.recycle();
            Parcel q = Parcel.obtain();
            q.unmarshall(raw, 0, raw.length);
            q.setDataPosition(0);
            String s = q.readString();
            q.recycle();
            if (!"marshalled".equals(s)) throw new IllegalStateException("got " + s);
            ok("[2e] Parcel.marshall/unmarshall (" + raw.length + " bytes)");
        } catch (Throwable t) {
            bad("[2e] Parcel.marshall/unmarshall", t);
        }
    }

    // ---------------------------------------------------------------- rung 3
    static void stepLooper() {
        // Looper.loop() calls Binder.clearCallingIdentity() on every message,
        // so the Binder natives are a hard prerequisite of the message loop,
        // not just of rung 4.
        if (!ensureRuntime("register_android_util_Log",
                            "register_android_os_SystemProperties",
                            "register_android_os_Binder",
                            "register_android_os_MessageQueue",
                            "register_android_os_SystemClock")) return;
        final AtomicInteger ran = new AtomicInteger();
        try {
            Looper.prepareMainLooper();
            Looper main = Looper.getMainLooper();
            if (main == null) throw new IllegalStateException("getMainLooper() == null");
            ok("[3a] Looper.prepareMainLooper() -> " + main + " (native MessageQueue::nativeInit)");

            // The main looper cannot be quit (Looper.quit() throws), so prove
            // enqueueing by inspecting the queue instead of spinning loop().
            Handler h = new Handler(main);
            h.post(ran::incrementAndGet);
            h.postDelayed(ran::incrementAndGet, 5);
            java.lang.reflect.Field f = Class.forName("android.os.MessageQueue")
                    .getDeclaredField("mMessages");
            f.setAccessible(true);
            Object head = f.get(main.getQueue());
            ok("[3b] Handler.post x2 to the MAIN looper -> queue head="
               + (head == null ? "null" : head.getClass().getSimpleName()));

            // A private looper can be quit, so it can be driven to completion.
            final Throwable[] err = new Throwable[1];
            Thread t = new Thread(() -> {
                try {
                    Looper.prepare();
                    Handler lh = new Handler(Looper.myLooper());
                    lh.post(ran::incrementAndGet);
                    lh.post(ran::incrementAndGet);
                    lh.postDelayed(ran::incrementAndGet, 50);  // NOT yet due => quitSafely drops it
                    lh.post(() -> Looper.myLooper().quitSafely());
                    Looper.loop();
                } catch (Throwable e) {
                    err[0] = e;
                }
            }, "e2d-looper");
            t.start();
            t.join(10000);
            if (err[0] != null) throw new RuntimeException("looper thread", err[0]);
            if (ran.get() != 2) throw new IllegalStateException("ran=" + ran.get());
            ok("[3c] private Looper: prepare + 2 posts + 1 delayed + quitSafely + loop() -> ran="
               + ran.get() + " (real semantics: quitSafely drops the not-yet-due delayed message;"
               + " epoll/eventfd + Binder.clearCallingIdentity, no IPC)");
        } catch (Throwable t) {
            bad("[3] Looper/Handler", t);
        }
    }

    // ---------------------------------------------------------------- rung 4
    static void stepBinder() {
        try {
            loadShim();
            System.out.println("PROBE " + Nat.binderProbe().trim().replace("\n", "\nPROBE "));
        } catch (Throwable t) {
            bad("[4a] raw /dev/binder probe", t);
        }

        if (!ensureRuntime("register_android_util_Log",
                            "register_android_os_SystemProperties",
                            "register_android_os_SystemClock",
                            "register_android_os_Parcel",
                            "register_android_os_Binder")) return;
        try {
            // The classic handle on ProcessState::self(): android.os.BinderInternal.
            // It is NOT in this image's boot classpath (measured: CNFE) -- it is
            // stripped from the shipped framework.jar dex.
            Object o = staticCall("android.os.BinderInternal", "getContextObject");
            ok("[4b] BinderInternal.getContextObject() -> " + o);
        } catch (Throwable t) {
            System.out.println("OBSERVED [4b] android.os.BinderInternal absent: " + t.getCause());
        }
        try {
            // ServiceManager -> getIServiceManager() -> ProcessState::self()
            //   -> open_driver(/dev/binder) -> getStrongProxyForHandle(0)
            //   -> IPCThreadState::talkWithDriver -> ioctl(BINDER_WRITE_READ)
            Object sm = staticCall("android.os.ServiceManager", "getIServiceManager");
            ok("[4b'] ServiceManager.getIServiceManager() -> " + sm
               + " (ProcessState::self() + getContextObject succeeded)");
        } catch (Throwable t) {
            bad("[4b'] ServiceManager.getIServiceManager()", t);
        }
        try {
            Object binder = staticCall("android.os.ServiceManager", "getService", "activity");
            ok("[4c] ServiceManager.getService(\"activity\") -> " + binder);
        } catch (Throwable t) {
            bad("[4c] ServiceManager.getService(activity)", t);
        }
    }

    static AssetManager newAssetManager() {
        try {
            java.lang.reflect.Constructor<?> c = AssetManager.class.getDeclaredConstructor();
            c.setAccessible(true);
            return (AssetManager) c.newInstance();
        } catch (Exception e) {
            throw new RuntimeException("new AssetManager()", e);
        }
    }

    // ---------------------------------------------------------------- rung 5
    static void stepResources() {
        if (!ensureRuntime("register_android_util_Log",
                            "register_android_os_SystemProperties",
                            "register_android_os_Trace",
                            "register_android_content_StringBlock",
                            "register_android_content_XmlBlock",
                            "register_android_content_AssetManager",
                            "register_android_content_res_ApkAssets",
                            "register_android_content_res_Configuration")) return;
        final String apk = System.getProperty("e2d.apk", "/data/apk/hello-views.apk");
        final String pkg = System.getProperty("e2d.pkg", "dev.acos.fixtures.helloviews");
        try {
            AssetManager am = newAssetManager();
            int c1 = (Integer) call(am, "addAssetPath", "/system/framework/framework-res.apk");
            int c2 = (Integer) call(am, "addAssetPath", apk);
            String[] top = am.list("");
            ok("[5a] AssetManager (real ApkAssets/AssetManager2): framework-res cookie=0x"
               + Integer.toHexString(c1) + " apk cookie=0x" + Integer.toHexString(c2)
               + " assets=" + java.util.Arrays.toString(top));
        } catch (Throwable t) {
            bad("[5a] AssetManager", t);
            return;
        }
        try {
            AssetManager am = newAssetManager();
            call(am, "addAssetPath", "/system/framework/framework-res.apk");
            call(am, "addAssetPath", apk);
            Resources r = new Resources(am, new DisplayMetrics(), new Configuration());
            int id = r.getIdentifier("app_name", "string", pkg);
            String v = id == 0 ? null : r.getString(id);
            ok("[5b] Resources(AssetManager,metrics,config): app_name id=0x"
               + Integer.toHexString(id) + " value=" + v);
            int hw = r.getIdentifier("main", "layout", pkg);
            System.out.println("PROBE  layout/main id=0x" + Integer.toHexString(hw));
            if (hw != 0) {
                android.content.res.XmlResourceParser xp = r.getLayout(hw);
                xp.next();
                ok("[5c] XmlResourceParser over res/layout: <" + xp.getName() + ">");
            } else {
                failures++;
                System.out.println("FAIL [5c] layout/main not found");
            }
        } catch (Throwable t) {
            bad("[5b] Resources", t);
        }
        try {
            Resources sys = Resources.getSystem();
            ok("[5d] Resources.getSystem() -> " + sys);
        } catch (Throwable t) {
            bad("[5d] Resources.getSystem() (needs ResourcesManager/ActivityThread)", t);
        }
    }

    // ---------------------------------------------------------------- rung 6
    static void stepActivity() {
        if (!ensureRuntime("register_android_util_Log",
                            "register_android_os_SystemProperties",
                            "register_android_os_Trace",
                            "register_android_content_StringBlock",
                            "register_android_content_XmlBlock",
                            "register_android_content_AssetManager",
                            "register_android_content_res_ApkAssets",
                            "register_android_content_res_Configuration",
                            "register_android_os_Binder",
                            "register_android_app_ActivityThread")) return;
        try {
            Object at = staticCall("android.app.ActivityThread", "systemMain");
            ok("[6a] ActivityThread.systemMain() -> " + at);
        } catch (Throwable t) {
            bad("[6a] ActivityThread.systemMain()", t);
        }
        try {
            Object at = staticCall("android.app.ActivityThread", "currentActivityThread");
            ok("[6b] ActivityThread.currentActivityThread() -> " + at);
            if (at != null) {
                Object ctx = call(at, "getSystemContext");
                ok("[6c] ActivityThread.getSystemContext() -> " + ctx);
            }
        } catch (Throwable t) {
            bad("[6b/6c] ActivityThread context", t);
        }
        try {
            Object app = staticCall("android.app.ActivityThread", "currentApplication");
            ok("[6d] currentApplication() -> " + app);
        } catch (Throwable t) {
            bad("[6d] currentApplication()", t);
        }
    }

    // ---------------------------------------------------------------- helpers
    /**
     * Two ways to get the real libandroid_runtime.so into the process:
     *   (a) System.load() -- goes through libnativeloader, which builds an app
     *       "classloader namespace" (clns-1) that only permits /data.  Every
     *       system library is then "not accessible for the namespace".  Kept
     *       because it *is* the first blocker, and worth recording.
     *   (b) LD_PRELOAD constructor -> dlopen RTLD_GLOBAL in the default
     *       namespace, which is what app_process/zygote does on device.
     */
    static boolean loadRuntime() {
        try {
            System.load("/system/lib64/libandroid_runtime.so");
            ok("[load] System.load(/system/lib64/libandroid_runtime.so)");
            return true;
        } catch (Throwable t) {
            System.out.println("NOTE System.load() failed (recorded blocker): "
                               + t.getMessage());
            System.out.println("NOTE   -> using the LD_PRELOAD default-namespace dlopen instead");
        }
        if (!loadShim()) return false;
        try {
            if (Nat.sym("register_android_os_Parcel") == 0) {
                throw new IllegalStateException("libandroid_runtime.so not loaded");
            }
            ok("[load] libandroid_runtime.so loaded in the default namespace by LD_PRELOAD");
            return true;
        } catch (Throwable t) {
            bad("[load] preloaded libandroid_runtime.so", t);
            return false;
        }
    }

    static boolean loadShim() {
        try {
            Nat.load();
            return true;
        } catch (Throwable t) {
            bad("System.load(libe2dshim.so)", t);
            return false;
        }
    }

    /**
     * Register framework natives in the order RuntimeInit's gRegJNI does.
     * Order matters: Log.isLoggable() is called by StrictMode's static
     * initializer, which register_android_os_Binder triggers (through its
     * JavaBinder class lookups); with Log unregistered that clinit throws and
     * leaves a pending exception that makes *every* subsequent JNI call in the
     * library return NULL (measured: 4 LOG_FATALs then SIGSEGV).
     */
    static boolean ensureRuntime(String... registerSymbols) {
        if (!loadRuntime()) return false;
        if (!loadShim()) return false;
        for (String sym : registerSymbols) {
            try {
                int rc = Nat.register(sym);
                if (rc == 0) {
                    System.out.println("SKIP " + sym + " (not exported by this libandroid_runtime)");
                    continue;
                }
                if (rc != 1) throw new IllegalStateException("register rc=" + rc);
                ok("registered " + sym);
            } catch (Throwable t) {
                bad("register " + sym, t);
                return false;
            }
        }
        return true;
    }
}
