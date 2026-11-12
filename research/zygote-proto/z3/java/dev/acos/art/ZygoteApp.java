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
        String sessions = System.getenv("ACOS_ZYGOTE_SESSIONS");
        if (sessions != null) {
            z4Main(sessions, t0, args);
            return;
        }
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
     * Z4 (research): N sessions forked from the same pre-initialised zygote.
     * Each session's contract is `<sessions>/<k>/{argv,binds}`, written by its
     * own `acos-art launch` (foreign-guest mode).  The child bind-mounts that
     * session's writable root, sockets and importer socket in a private mount
     * namespace, then applies the production seccomp/Landlock guard (the same
     * `libacos_seccomp_guest.so` the normal path preloads) and re-enters the
     * unmodified `LaunchMain.main` with that session's argv.
     */
    private static void z4Main(String sessions, long t0, String[] args) throws Throwable {
        int n = 1;
        String ns = System.getenv("ACOS_ZYGOTE_N");
        if (ns != null) {
            try {
                n = Integer.parseInt(ns.trim());
            } catch (NumberFormatException ignored) {
            }
        }
        say("Z4 sessions=" + sessions + " n=" + n);
        String[][] argvs = new String[n][];
        for (int k = 0; k < n; k++) {
            argvs[k] = readArgv(sessions + "/" + k + "/argv");
            say("Z4 session tag=" + k + " argv_args=" + argvs[k].length);
        }
        int[] pids = new int[n];
        for (int k = 0; k < n; k++) {
            ZygoteHooks.preFork();
            long tFork = now();
            int pid = dev.acos.z1.Nat.fork();
            if (pid == 0) {
                long f1 = now();
                // AOSP order: namespaces/mounts first, while this process is
                // still single-threaded (`unshare(CLONE_NEWUSER)` is EINVAL for
                // a multithreaded caller; postForkChild restarts ART's threads).
                int rc = dev.acos.z1.Nat.spawnSession(k);
                if (rc != 0) {
                    say("Z4 child tag=" + k + " sandbox_fail rc=" + rc);
                    System.exit(70);
                }
                ZygoteHooks.postForkChild(0, false, false, null);
                say("Z4 child tag=" + k + " pid=" + dev.acos.z1.Nat.getpid()
                        + " fork_to_sandbox_ms=" + (now() - f1) / 1000000L
                        + " userns=" + dev.acos.z1.Nat.sandboxRc(0)
                        + " mntns=" + dev.acos.z1.Nat.sandboxRc(1)
                        + " pidns=" + dev.acos.z1.Nat.sandboxRc(2)
                        + " binds_rc=" + dev.acos.z1.Nat.sandboxRc(4)
                        + " binds=" + dev.acos.z1.Nat.sandboxRc(5)
                        + " pid_in_ns=" + dev.acos.z1.Nat.sandboxRc(7)
                        + " threads=" + threads());
                long g0 = now();
                // The production guest guard, in AOSP order: after the mounts,
                // before ART starts any app thread.
                System.load("/data/seccomp/libacos_seccomp_guest.so");
                say("Z4 child tag=" + k + " guard_ms=" + (now() - g0) / 1000000L
                        + " seccomp=" + dev.acos.z1.Nat.seccompStatus());
                // ZS/Z4 security evidence (research only): what this child can
                // see and signal. Recorded, not mitigated (see Z4.md).
                if (System.getenv("ACOS_ZYGOTE_PROBE") != null) {
                    securityProbe(sessions, k, n);
                }
                startDaemons();
                say("Z4 child tag=" + k + " fork_to_ready_ms=" + (now() - tFork) / 1000000L
                        + " threads=" + threads());
                LaunchMain.main(argvs[k]);
                return;
            }
            ZygoteHooks.postForkCommon();
            pids[k] = pid;
            writeText(sessions + "/" + k + "/pid", Integer.toString(pid));
            writeText(sessions + "/zygote.pid", Integer.toString(dev.acos.z1.Nat.getpid()));
            say("Z4 forked tag=" + k + " child=" + pid + " threads=" + threads()
                    + " total_ms=" + (now() - t0) / 1000000L);
        }
        for (int k = 0; k < n; k++) {
            long st = dev.acos.z1.Nat.waitPid(pids[k]);
            int status = (int) st;
            say("Z4 reaped tag=" + k + " pid=" + pids[k]
                    + " wait4_ret=" + (int) (st >> 32)
                    + " status=0x" + Integer.toHexString(status)
                    + " exit=" + ((status >> 8) & 0xff)
                    + " sig=" + (status & 0x7f));
        }
        System.exit(0);
    }

    private static void writeText(String path, String text) {
        try {
            java.io.FileWriter w = new java.io.FileWriter(path);
            try {
                w.write(text + "\n");
            } finally {
                w.close();
            }
        } catch (Throwable ignored) {
        }
    }

    private static int readInt(String path) {
        try {
            BufferedReader in = new BufferedReader(new FileReader(path));
            try {
                String line = in.readLine();
                return line == null ? -1 : Integer.parseInt(line.trim());
            } finally {
                in.close();
            }
        } catch (Throwable t) {
            return -1;
        }
    }

    private static String readAll(String path) {
        try {
            BufferedReader in = new BufferedReader(new FileReader(path));
            StringBuilder b = new StringBuilder();
            try {
                String line;
                while ((line = in.readLine()) != null) b.append(line).append(' ');
            } finally {
                in.close();
            }
            return b.toString().trim();
        } catch (Throwable t) {
            return "ERR:" + t.getClass().getSimpleName();
        }
    }

    /**
     * ZS/Z4 security evidence at N&gt;1: process visibility, signals, session
     * sockets and cgroup identity, as seen *from inside* one forked child.
     * Results are printed as one `Z4SEC` line and recorded in Z4.md; nothing is
     * hidden or fixed here.
     */
    private static void securityProbe(String sessions, int tag, int n) {
        // Wait for the siblings' pid files (the parent forks them after us).
        int zygote = -1;
        int[] pids = new int[n];
        for (int tries = 0; tries < 100; tries++) {
            zygote = readInt(sessions + "/zygote.pid");
            boolean all = zygote > 0;
            for (int j = 0; j < n; j++) {
                pids[j] = readInt(sessions + "/" + j + "/pid");
                if (pids[j] <= 0) all = false;
            }
            if (all) break;
            try {
                Thread.sleep(50);
            } catch (InterruptedException ignored) {
            }
        }
        int sibling = -1;
        for (int j = 0; j < n; j++) {
            if (j != tag && pids[j] > 0) {
                sibling = pids[j];
                break;
            }
        }
        boolean procZygote = new java.io.File("/proc/" + zygote).exists();
        boolean fdsZygote = new java.io.File("/proc/" + zygote + "/fd").exists();
        boolean procSibling = sibling > 0 && new java.io.File("/proc/" + sibling).exists();
        boolean fdsSibling = sibling > 0 && new java.io.File("/proc/" + sibling + "/fd").exists();
        int killSibling = sibling > 0 ? dev.acos.z1.Nat.kill(sibling, 0) : -999;
        int killZygote = zygote > 0 ? dev.acos.z1.Nat.kill(zygote, 0) : -999;
        boolean planVisible = new java.io.File("/sessions").exists();
        String sibSock = "n/a";
        for (int j = 0; j < n; j++) {
            if (j == tag) continue;
            String p = sessions + "/" + j + "/sock/peer.sock";
            sibSock = p + ":exists=" + new java.io.File(p).exists()
                    + ",read=" + new java.io.File(p).canRead()
                    + ",write=" + new java.io.File(p).canWrite();
            break;
        }
        String cgroup = readAll("/proc/self/cgroup");
        String nspid = readAll("/proc/self/status");
        int idx = nspid.indexOf("NSpid:");
        String nspidLine = idx >= 0 ? nspid.substring(idx, Math.min(nspid.length(), idx + 60)) : "n/a";
        int oomGroup = -1;
        try {
            String cg = cgroup.replace("0::", "").trim();
            String p = "/sys/fs/cgroup" + cg + "/memory.oom.group";
            oomGroup = readInt(p);
        } catch (Throwable ignored) {
        }
        say("Z4SEC tag=" + tag + " zygote_pid_file=" + zygote + " sibling=" + sibling
                + " proc_zygote=" + procZygote + " proc_zygote_fd=" + fdsZygote
                + " proc_sibling=" + procSibling + " proc_sibling_fd=" + fdsSibling
                + " kill_sibling=" + killSibling + " kill_zygote=" + killZygote
                + " plan_visible=" + planVisible + " sibling_sock=" + sibSock
                + " ns_pid=" + nspidLine.replace(' ', '_')
                + " cgroup=" + cgroup.replace(' ', '_')
                + " oom_group=" + oomGroup);
    }

    /** One argv element per line; the launcher writes it before spawning us. */
    private static String[] readArgv(String path) throws Throwable {
        long deadline = now() + 90000000000L;
        while (true) {
            try {
                java.util.List<String> out = new java.util.ArrayList<>();
                BufferedReader in = new BufferedReader(new FileReader(path));
                try {
                    String line;
                    while ((line = in.readLine()) != null) out.add(line);
                } finally {
                    in.close();
                }
                if (!out.isEmpty()) return out.toArray(new String[0]);
            } catch (Throwable t) {
                if (now() > deadline) throw t;
            }
            Thread.sleep(50);
        }
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
