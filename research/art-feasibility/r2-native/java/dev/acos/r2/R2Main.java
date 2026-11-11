package dev.acos.r2;

import java.lang.reflect.Constructor;
import java.lang.reflect.Field;
import java.lang.reflect.InvocationTargetException;
import java.lang.reflect.Method;

/**
 * R2: does an APK's *own* native library load and run through the normal app
 * path on real ART?
 *
 * The whole point is that nothing here is app-specific C: we build the same
 * thing the framework builds for an installed package —
 *
 *     PathClassLoader(dexPath=<base.apk>, librarySearchPath=<.../lib/x86_64>,
 *                     parent=boot)
 *
 * — and then touch only the app's own Java surface. The first touch of the app
 * class runs its static initialiser, which is where the APK's own
 * `System.loadLibrary("hellojni")` lives, so the library is loaded by *the app's*
 * classloader, not by our harness classloader, and therefore lands in the
 * libnativeloader `clns` namespace with the app's native library directory as
 * its search path.
 *
 *   R2Main hello <apk> <libdir>
 *   R2Main rn    <apk> <libdir>
 */
public final class R2Main {

    static int pass = 0;
    static int fail = 0;

    static void ok(String what, Object value) {
        pass++;
        System.out.println("PASS " + what + " :: " + value);
    }

    static void bad(String what, Object value) {
        fail++;
        System.out.println("FAIL " + what + " :: " + value);
    }

    static void info(String what) {
        System.out.println("INFO " + what);
    }

    public static void main(String[] args) throws Exception {
        String mode = args.length > 0 ? args[0] : "hello";
        String apk = args.length > 1 ? args[1] : null;
        String libDir = args.length > 2 ? args[2] : null;
        String dexPath = args.length > 3 ? args[3] : null;
        info("mode=" + mode + " apk=" + apk + " libdir=" + libDir);
        info("java.library.path=" + System.getProperty("java.library.path"));
        info("system classloader=" + ClassLoader.getSystemClassLoader());
        info("boot classloader parent of system cl=" + ClassLoader.getSystemClassLoader().getParent());

        setTargetSdk(34);

        ClassLoader appCl = makeAppClassLoader(apk, libDir);
        info("app classloader=" + appCl + " parent=" + appCl.getParent());

        if (mode.startsWith("hello")) {
            if (mode.endsWith("raw")) {
                info("shim: NOT loaded (raw run)");
            } else {
                try {
                    Nat.load();
                    ok("System.load(/data/lib/libr2shim.so)", "ok");
                    int rc = Nat.register("register_android_util_Log");
                    if (rc == 1) ok("register_android_util_Log", "called");
                    else bad("register_android_util_Log", "rc=" + rc);
                } catch (Throwable t) {
                    bad("shim", chain(t));
                }
            }
            hello(appCl);
        } else if ("rn".equals(mode)) {
            rn(appCl, dexPath, libDir);
        } else {
            System.out.println("unknown mode " + mode);
        }

        System.out.println("RESULT pass=" + pass + " fail=" + fail);
        System.out.flush();
        // dalvikvm keeps non-daemon threads alive; make the outcome explicit.
        Runtime.getRuntime().halt(fail == 0 ? 0 : 1);
    }

    // ---------------------------------------------------------------- helpers

    static ClassLoader makeAppClassLoader(String apk, String libDir) throws Exception {
        // PathClassLoader(String dexPath, String librarySearchPath, ClassLoader parent)
        // parent == null means the boot class loader: exactly what
        // LoadedApk#getClassLoader does for the app class loader.
        Class<?> pcl = Class.forName("dalvik.system.PathClassLoader");
        Constructor<?> c = pcl.getConstructor(String.class, String.class, ClassLoader.class);
        return (ClassLoader) c.newInstance(apk, libDir, null);
    }

    /**
     * A secondary classloader over our own dex whose *library* search path is
     * the APK's nativeLibraryDirectory:
     * {@code PathClassLoader(dexPath, librarySearchPath, parent)}.
     * (The 4-argument {@code DexClassLoader} ctor is a trap here: its String
     * arguments are (dexPath, optimizedDirectory, librarySearchPath, parent), so
     * passing the library dir second silently drops it -- measured.)
     */
    static final class DexClassLoaderHack {
        ClassLoader make(String dexPath, String libDir, ClassLoader parent) throws Exception {
            Class<?> c = Class.forName("dalvik.system.PathClassLoader");
            return (ClassLoader) c.getConstructor(String.class, String.class, ClassLoader.class)
                    .newInstance(dexPath, libDir, parent);
        }
    }

    static Object newAppInstance(Class<?> cls) throws Exception {
        Constructor<?> c = cls.getDeclaredConstructor();
        c.setAccessible(true);
        return c.newInstance();
    }

    /** sun.misc.Unsafe.allocateInstance: a field-initialiser-free instance. */
    static Object allocate(Class<?> cls) throws Exception {
        Class<?> u = Class.forName("sun.misc.Unsafe");
        Field f = u.getDeclaredField("theUnsafe");
        f.setAccessible(true);
        Object unsafe = f.get(null);
        return u.getMethod("allocateInstance", Class.class).invoke(unsafe, cls);
    }

    /**
     * The device sets the app's targetSdkVersion before the first
     * System.loadLibrary (LoadedApk -> VMRuntime), and libnativeloader filters
     * the exposed public libraries by it. Without this, clns is configured with
     * target_sdk_version=0, which is *more* permissive (<=30 gets the extended
     * list), so it can hide a blocker but cannot invent one.
     */
    static void setTargetSdk(int sdk) {
        try {
            Class<?> vm = Class.forName("dalvik.system.VMRuntime");
            Object rt = vm.getMethod("getRuntime").invoke(null);
            vm.getMethod("setTargetSdkVersion", int.class).invoke(rt, sdk);
            ok("VMRuntime.setTargetSdkVersion(" + sdk + ")", "applied");
        } catch (Throwable t) {
            info("VMRuntime.setTargetSdkVersion(" + sdk + ") unavailable :: " + chain(t));
        }
    }

    static Method method(Class<?> cls, String name, Class<?>... sig) throws Exception {
        Method m = cls.getDeclaredMethod(name, sig);
        m.setAccessible(true);
        return m;
    }

    static Object field(Object target, String name) throws Exception {
        Field f = target.getClass().getDeclaredField(name);
        f.setAccessible(true);
        return f.get(target);
    }

    // -------------------------------------------------------------- hello-jni

    static void hello(ClassLoader appCl) throws Exception {
        // 1. The app class. Its <clinit> is `System.loadLibrary("hellojni")`,
        //    so this single Class.forName is the entire native-load step.
        Class<?> ma;
        try {
            ma = Class.forName("dev.acos.fixtures.hellojni.MainActivity", true, appCl);
            ok("app-class-load (runs System.loadLibrary(\"hellojni\"))", ma + " cl=" + ma.getClassLoader());
        } catch (Throwable t) {
            bad("app-class-load", chain(t));
            return;
        }

        Object act;
        try {
            act = newAppInstance(ma);
            ok("app-instance (Activity default ctor, no ActivityThread)", act.getClass().getName());
        } catch (Throwable t) {
            // Expected on stock ART with no Looper: Activity's field
            // initialisers build a Handler (HostCallbacks).
            info("app-instance via `new` blocked :: " + chain(t));
            try {
                act = allocate(ma);
                ok("app-instance (Unsafe.allocateInstance)", act.getClass().getName());
            } catch (Throwable t2) {
                bad("app-instance", chain(t2));
                return;
            }
        }

        // 2. The APK's own JNI methods, in the fixture's own call order.
        StringBuilder sb = new StringBuilder();
        try {
            Object s = method(ma, "stringFromJNI").invoke(act);
            sb.append("stringFromJNI=").append(s).append('\n');
        } catch (Throwable t) {
            bad("stringFromJNI", chain(t));
        }
        try {
            Object v = method(ma, "add", int.class, int.class).invoke(act, 20, 22);
            sb.append("add(20,22)=").append(v).append('\n');
        } catch (Throwable t) {
            bad("add", chain(t));
        }
        try {
            Object v = method(ma, "sumArray", int[].class).invoke(act, (Object) new int[] {1, 2, 3, 4, 5});
            sb.append("sumArray([1,2,3,4,5])=").append(v).append('\n');
        } catch (Throwable t) {
            bad("sumArray", chain(t));
        }
        // 3. Native -> Java callbacks (CallStaticIntMethod + CallVoidMethod).
        try {
            method(ma, "callBack").invoke(act);
            Object cb = field(act, "callbackResult");
            Object vd = field(act, "voidResult");
            sb.append("javaCallbackResult=").append(cb).append('\n');
            sb.append("voidCallback=").append(vd).append('\n');
        } catch (Throwable t) {
            bad("callBack", chain(t));
        }
        // 4. The method registered by RegisterNatives in JNI_OnLoad.
        try {
            Object v = method(ma, "registeredEcho", String.class).invoke(act, "echo!");
            sb.append("registeredEcho=").append(v).append('\n');
        } catch (Throwable t) {
            bad("registeredEcho", chain(t));
        }
        // 5. ThrowNew.
        try {
            method(ma, "throwFromJNI").invoke(act);
            sb.append("throwFromJNI=no-exception\n");
        } catch (InvocationTargetException e) {
            Throwable c = e.getCause();
            sb.append("throwFromJNI caught=").append(c.getMessage()).append('\n');
            if (!(c instanceof IllegalStateException)) {
                bad("throwFromJNI type", c.getClass().getName());
            }
        } catch (Throwable t) {
            bad("throwFromJNI", chain(t));
        }

        String report = sb.toString();
        System.out.println("---- app report (fixture format) ----");
        System.out.print(report);
        System.out.println("---- end app report ----");

        // The exact strings scenarios/hello-jni.yaml asserts on.
        expect(report, "stringFromJNI=Hello from native C");
        expect(report, "add(20,22)=42");
        expect(report, "sumArray([1,2,3,4,5])=15");
        expect(report, "javaCallbackResult=42");
        expect(report, "voidCallback=from-native");
        expect(report, "registeredEcho=echo!");
        expect(report, "throwFromJNI caught=thrown from native");

        // 6. How far does the *real* onCreate get on stock ART with no
        //    ActivityThread? (V1 comparison: the fixture's report is produced in
        //    onCreate, so this is the delta between "native works" and "the
        //    fixture's own entry point works".)
        try {
            Method on = method(ma, "onCreate", Class.forName("android.os.Bundle"));
            on.invoke(act, new Object[] {null});
            ok("onCreate(Bundle=null)", "returned");
        } catch (Throwable t) {
            info("onCreate(Bundle=null) blocked :: " + chain(t));
        }
    }

    static void expect(String haystack, String needle) {
        if (haystack.contains(needle)) {
            ok("v1-expectation " + needle, "present");
        } else {
            bad("v1-expectation " + needle, "MISSING");
        }
    }

    // ---------------------------------------------------------------- rn-hello

    static void rn(ClassLoader appCl, String dexPath, String libDir) throws Exception {
        // A loader whose library search path is the APK's nativeLibraryDirectory,
        // so System.loadLibrary runs in the *app's* namespace (see RnLoader).
        ClassLoader libCl = new DexClassLoaderHack().make(dexPath, libDir, appCl);
        info("loader for System.loadLibrary=" + libCl + " parent=" + libCl.getParent());
        Method viaLoader = Class.forName("dev.acos.r2.RnLoader", true, libCl)
                .getMethod("load", String.class);
        // Same libraries an RN app's `SoLoader.loadLibrary` call would pull in,
        // in the order libreactnative's DT_NEEDED closure wants them.
        String[] libs = {"c++_shared", "jsi", "fbjni", "hermes", "reactnative", "hermestooling"};
        for (String lib : libs) {
            long t0 = System.nanoTime();
            try {
                viaLoader.invoke(null, lib);
                long us = (System.nanoTime() - t0) / 1000;
                ok("loadLibrary(\"" + lib + "\")", us + " us");
            } catch (Throwable t) {
                bad("loadLibrary(\"" + lib + "\")", chain(t));
            }
        }
        // Proof that the *registered* natives really dispatch: Yoga's JNI
        // entry points are registered by libyoga_so's JNI_OnLoad, which
        // libreactnative's merged JNI_OnLoad calls. No JS, no Activity.
        try {
            Class<?> y = Class.forName("com.facebook.yoga.YogaNative", true, appCl);
            Method newCfg = y.getDeclaredMethod("jni_YGConfigNewJNI");
            Method freeCfg = y.getDeclaredMethod("jni_YGConfigFreeJNI", long.class);
            Object cfg = newCfg.invoke(null);
            ok("YogaNative.jni_YGConfigNewJNI()", cfg + " (native ptr)");
            freeCfg.invoke(null, cfg);
            ok("YogaNative.jni_YGConfigFreeJNI(ptr)", "returned");
        } catch (Throwable t) {
            bad("YogaNative registered native", chain(t));
        }

        // Can the RN Java surface be touched (i.e. is the dex usable on this
        // ART, independent of the native side)?
        String[] classes = {
            "com.facebook.jni.HybridData",
            "com.facebook.soloader.SoLoader",
            "com.facebook.react.bridge.JavaModuleWrapper",
            "com.facebook.react.bridge.ReactContext",
        };
        for (String cn : classes) {
            try {
                Class<?> c = Class.forName(cn, false, appCl);
                ok("app-class " + cn, c.getClassLoader());
            } catch (Throwable t) {
                bad("app-class " + cn, chain(t));
            }
        }
    }

    // ------------------------------------------------------------------ misc

    static String chain(Throwable t) {
        StringBuilder sb = new StringBuilder();
        while (t != null) {
            if (sb.length() > 0) sb.append(" <- ");
            sb.append(t.getClass().getName());
            if (t.getMessage() != null) sb.append(": ").append(t.getMessage());
            if (t instanceof InvocationTargetException) {
                t = ((InvocationTargetException) t).getCause();
                continue;
            }
            Throwable n = t.getCause();
            if (n == null) break;
            t = n;
        }
        return sb.toString();
    }
}
