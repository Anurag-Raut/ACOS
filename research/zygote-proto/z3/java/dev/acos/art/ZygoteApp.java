package dev.acos.art;

import dalvik.system.ZygoteHooks;
import java.io.BufferedReader;
import java.io.FileReader;
import java.lang.reflect.Method;

/**
 * Z3 (research only, never the production guest): a Zygote-shaped launcher for
 * the **real** ACOS launch guest.
 *
 * <p>The parent does the app-independent part of
 * {@code LaunchMain.run()} once — framework JNI registration, the binder-driver
 * load and the system {@code AssetManager} + static-RRO setup — inside the
 * `ZygoteHooks` no-thread window, freezes the heap with
 * {@code ZygoteHooks.gcAndFinalize()}, and then forks one child per requested
 * app. The child re-enters the *unmodified* {@link LaunchMain#main(String[])}
 * with the same argv `acos-art launch` built, so everything app-specific (the
 * APK class loader, {@code Application}/{@code Activity}, the per-session peer
 * connection to {@code ServiceHost}, the binder looper, the main {@code Looper}
 * and the display buffers) happens after the fork in the child, exactly as on
 * the normal exec path.
 *
 * <p>argv is the production guest argv (see {@code crates/acos-art/src/launch.rs}):
 * {@code apk package outDir until activity appClass theme overlays …}.
 */
public final class ZygoteApp {
    private static int preloaded;

    public static void main(String[] args) throws Throwable {
        long t0 = now();
        int sig = dev.acos.z1.Nat.resetSigchld();
        say("Z3 zygote_pid=" + dev.acos.z1.Nat.getpid()
                + " threads=" + threads()
                + " sigchld_reset=" + sig
                + " args=" + args.length
                + " heap=" + heap());

        // LaunchMain parses the same argv after the fork; the one field the
        // pre-fork system-Resources step needs is the static-RRO idmap list.
        if (args.length > 7 && args[7] != null) {
            LaunchMain.sOverlayIdmaps = args[7];
        }

        // ---- 1. the no-thread window (ZygoteInit:768) -----------------------
        ZygoteHooks.startZygoteNoThreadCreation();
        say("Z3 no_thread_section=on threads=" + threads());

        // ---- 2. app-independent framework init ------------------------------
        long a = now();
        LaunchMain.registerNatives();
        LaunchMain.applySystemOverlays();
        // The binder driver .so (no thread yet: startBinderLooper is per child).
        Nat.load();
        preloadFrameworkClasses();
        long b = now();
        say("Z3 preapp_init ms=" + (b - a) / 1000000L
                + " preloaded=" + preloaded + " threads=" + threads()
                + " heap=" + heap());

        // ---- 3. shrink the pre-fork heap (ZygoteInit:428) -------------------
        ZygoteHooks.gcAndFinalize();
        long c = now();
        say("Z3 post_preload_gc ms=" + (c - b) / 1000000L
                + " threads=" + threads() + " heap=" + heap());
        ZygoteHooks.stopZygoteNoThreadCreation();
        say("Z3 no_thread_section=off threads=" + threads());

        // ---- 4. freeze, fork, one app child --------------------------------
        ZygoteHooks.preFork();
        long tFork = now();
        say("Z3 after_preFork ms=" + (tFork - c) / 1000000L + " threads=" + threads());
        int pid = dev.acos.z1.Nat.fork();
        if (pid == 0) {
            // ---- child: everything app-specific happens from here ----------
            ZygoteHooks.postForkChild(0, false, false, null);
            long f1 = now();
            say("Z3 child pid=" + dev.acos.z1.Nat.getpid()
                    + " fork_to_post_fork_ms=" + (f1 - tFork) / 1000000L
                    + " threads=" + threads());
            // A zygote never runs Runtime::StartDaemonThreads (runtime.cc:1350);
            // the child must, or it dies inside finalization (Z1 §6.2).
            startDaemons();
            say("Z3 child pid=" + dev.acos.z1.Nat.getpid()
                    + " daemons_started_ms=" + (now() - f1) / 1000000L
                    + " threads=" + threads());
            // The unmodified production guest entry, same argv.
            LaunchMain.main(args);
            return;
        }
        // ---- parent --------------------------------------------------------
        ZygoteHooks.postForkCommon();
        say("Z3 forked child=" + pid + " threads=" + threads());
        long st = dev.acos.z1.Nat.waitPid(pid);
        int ret = (int) (st >> 32);
        int status = (int) st;
        say("Z3 reaped child=" + pid + " wait4_ret=" + ret
                + " status=0x" + Integer.toHexString(status)
                + " exit=" + ((status >> 8) & 0xff)
                + " sig=" + (status & 0x7f)
                + " total_ms=" + (now() - t0) / 1000000L);
        System.exit(ret > 0 && status == 0 ? 0 : 1);
    }

    /**
     * ZygoteInit.preloadClasses, reduced to the framework classes this launch
     * touches: resolved (not initialised) before the fork so every child
     * inherits the image/class table through CoW. Failures are counted, not
     * fatal, exactly as {@code ZygoteInit} logs and continues.
     */
    private static void preloadFrameworkClasses() {
        String[] names = {
            "android.app.ActivityThread",
            "android.app.Application",
            "android.app.Instrumentation",
            "android.app.ApplicationThread",
            "android.app.servertransaction.ClientTransaction",
            "android.app.servertransaction.LaunchActivityItem",
            "android.app.servertransaction.ResumeActivityItem",
            "android.app.servertransaction.PauseActivityItem",
            "android.app.servertransaction.StopActivityItem",
            "android.app.servertransaction.DestroyActivityItem",
            "android.content.Context",
            "android.content.Intent",
            "android.content.ComponentName",
            "android.content.pm.ApplicationInfo",
            "android.content.pm.ActivityInfo",
            "android.content.pm.InstrumentationInfo",
            "android.content.pm.ProviderInfoList",
            "android.content.res.Configuration",
            "android.content.res.CompatibilityInfo",
            "android.content.res.AssetManager",
            "android.content.res.Resources",
            "android.content.res.ApkAssets",
            "android.os.Looper",
            "android.os.MessageQueue",
            "android.os.Parcel",
            "android.os.Bundle",
            "android.os.ServiceManager",
            "android.os.LocaleList",
            "android.view.View",
            "android.view.ViewRootImpl",
            "android.view.DisplayInfo",
            "android.view.Choreographer",
            "android.view.WindowManagerGlobal",
            "android.graphics.Canvas",
            "android.widget.TextView",
        };
        for (String n : names) {
            try {
                Class.forName(n, false, null);
                preloaded++;
            } catch (Throwable t) {
                say("Z3 preload_fail class=" + n + " err=" + t);
            }
        }
    }

    private static void startDaemons() {
        try {
            Class<?> d = Class.forName("java.lang.Daemons");
            Method m = d.getDeclaredMethod("start");
            m.setAccessible(true);
            m.invoke(null);
        } catch (Throwable t) {
            say("Z3 daemons_fail " + t);
        }
    }

    private static int threads() {
        try {
            BufferedReader in = new BufferedReader(new FileReader("/proc/self/status"));
            try {
                String line;
                while ((line = in.readLine()) != null) {
                    if (line.startsWith("Threads:")) {
                        return Integer.parseInt(line.substring(8).trim());
                    }
                }
            } finally {
                in.close();
            }
        } catch (Throwable ignored) {
        }
        return -1;
    }

    private static String heap() {
        Runtime r = Runtime.getRuntime();
        return "used_kb=" + ((r.totalMemory() - r.freeMemory()) >> 10)
                + " total_kb=" + (r.totalMemory() >> 10);
    }

    private static long now() {
        return dev.acos.z1.Nat.nanos();
    }

    private static void say(String s) {
        System.out.println(s);
        System.out.flush();
    }
}
