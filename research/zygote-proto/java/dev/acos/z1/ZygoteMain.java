package dev.acos.z1;

import android.system.Os;
import dalvik.system.ZygoteHooks;
import java.io.BufferedReader;
import java.io.FileReader;

/**
 * Z2: a standalone Zygote-shaped parent, Google mechanisms only, with an
 * in-process sandbox in each forked child.
 *
 * <p>Order copied from {@code ZygoteInit.main} and {@code ZygoteConnection.
 * processOneCommand}:
 *
 * <pre>
 *   startZygoteNoThreadCreation  ->  preload  ->  gcAndFinalize
 *   ->  stopZygoteNoThreadCreation
 *   ->  per child: preFork -> fork
 *        -> [child]  Nat.spawnSandboxed(tag)   (NEWUSER/NEWNS/NEWPID, mounts, seccomp,
 *                                               caps, setresuid -- AOSP order)
 *                    postForkChild
 *        -> [parent] postForkCommon
 * </pre>
 *
 * <p>Args: {@code <children> <holdMs> [preloadLimit]}.
 */
public final class ZygoteMain {
    private static int preloadLimit = 4000;
    private static int preloadOk = 0;
    private static int preloadFail = 0;

    public static void main(String[] args) throws Exception {
        int n = args.length > 0 ? Integer.parseInt(args[0]) : 2;
        int holdMs = args.length > 1 ? Integer.parseInt(args[1]) : 8000;
        if (args.length > 2) {
            preloadLimit = Integer.parseInt(args[2]);
        }
        long t0 = Nat.nanos();
        int sig = Nat.resetSigchld();
        say("sigchld_reset rc=" + sig + " action=0x"
                + Long.toHexString(Nat.sigchldAction())
                + " SigIgn=" + Work.statusMask("SigIgn")
                + " SigCgt=" + Work.statusMask("SigCgt")
                + " SigBlk=" + Work.statusMask("SigBlk"));
        say("parent pid=" + Nat.getpid() + " vm_start_ns=" + t0
                + " threads=" + Work.threadsInProc());
        Work.printParentNs("parent");
        say("parent_private path=/data/zygote-private/secret.txt "
                + Work.readProbe("/data/zygote-private/secret.txt"));

        // 1. the no-thread window (ZygoteInit:768)
        ZygoteHooks.startZygoteNoThreadCreation();
        say("no_thread_section=on threads=" + Work.threadsInProc());

        // 2. preload
        long p0 = Nat.nanos();
        preload();
        long p1 = Nat.nanos();
        say("preload ok=" + preloadOk + " failed=" + preloadFail
                + " ms=" + (p1 - p0) / 1000000L + " " + Work.heapLine());

        // 3. shrink the shared image before it is frozen (ZygoteInit:428/846)
        ZygoteHooks.gcAndFinalize();
        long p2 = Nat.nanos();
        say("post_preload_gc ms=" + (p2 - p1) / 1000000L + " " + Work.heapLine());
        ZygoteHooks.stopZygoteNoThreadCreation();
        say("no_thread_section=off threads=" + Work.threadsInProc());

        // Value the children must inherit through the fork, not recompute.
        Work.INHERITED = 42;
        say("parent_set constant inherited=" + Work.INHERITED
                + " ihash_String=" + System.identityHashCode(String.class)
                + " ihash_Context=" + System.identityHashCode(android.content.Context.class));

        if (n < 1) {
            n = 1;
        }
        int[] kids = new int[n];
        long[] forkNs = new long[n];
        for (int i = 0; i < n; i++) {
            long fp0 = Nat.nanos();
            // 4. freeze + stop the world (Heap::PreZygoteFork), then fork
            ZygoteHooks.preFork();
            long fp1 = Nat.nanos();
            if (i == 0) {
                say("after_preFork threads=" + Work.threadsInProc()
                        + " preFork_ms=" + (fp1 - fp0) / 1000000L + " " + Work.heapLine());
            }
            long tFork = Nat.nanos();
            int pid = Nat.fork();
            if (pid == 0) {
                // ---- child: A = sandbox launcher, returns only in the app ----
                long s0 = Nat.nanos();
                int src = Nat.spawnSandboxed(i + 1);
                long s1 = Nat.nanos();
                if (src != 0) {
                    System.out.println("sandbox_spawn_failed tag=" + (i + 1) + " rc=" + src);
                    System.out.flush();
                }
                ZygoteHooks.postForkChild(0, /* isSystemServer= */ false,
                        /* isZygote= */ false, /* instructionSet= */ null);
                long c1 = Nat.nanos();
                System.out.println("post_fork_child pid=" + Nat.getpid() + " ms="
                        + (c1 - s1) / 1000000L + " sandbox_ms=" + (s1 - s0) / 1000000L
                        + " fork_to_post_fork_ms=" + (c1 - tFork) / 1000000L
                        + " threads=" + Work.threadsInProc());
                System.out.flush();
                Work.run(i + 1, "forked", tFork);
                Thread.sleep(holdMs);
                int mod = (i + 1) % 3;
                if (mod == 1) {
                    System.out.println("child_exit pid=" + Nat.getpid() + " tag=" + (i + 1)
                            + " mode=exit code=7");
                    System.out.flush();
                    System.exit(7);
                } else if (mod == 2) {
                    System.out.println("child_exit pid=" + Nat.getpid() + " tag=" + (i + 1)
                            + " mode=signal sig=15");
                    System.out.flush();
                    Os.kill(Nat.getpid(), 15 /* SIGTERM */);
                    Thread.sleep(500);
                    System.out.println("kill_failed pid=" + Nat.getpid() + " tag=" + (i + 1));
                    System.out.flush();
                    System.exit(9);
                }
                System.out.println("child_exit pid=" + Nat.getpid() + " tag=" + (i + 1)
                        + " mode=exit code=0");
                System.out.flush();
                System.exit(0);
            }
            // ---- parent ----
            forkNs[i] = tFork;
            ZygoteHooks.postForkCommon();
            kids[i] = pid;
            say("forked child=" + pid + " tag=" + (i + 1)
                    + " fork_ns=" + tFork
                    + " postForkCommon_ms=" + (Nat.nanos() - tFork) / 1000000L
                    + " threads=" + Work.threadsInProc());
        }

        // The child prints its own fork->ready time (it inherits tFork).
        say("HOLD_START children=" + n + " hold_ms=" + holdMs
                + " pids=" + join(kids) + " parent=" + Nat.getpid());
        Thread.sleep(holdMs);
        for (int i = 0; i < n; i++) {
            say("reaped child=" + kids[i] + " " + waited(Nat.waitPid(kids[i])));
        }
        say("HOLD_END children=" + n + " " + Work.heapLine());

        // 5. The parent must still be able to fork after forks + GCs + threads.
        ZygoteHooks.preFork();
        long lateFork = Nat.nanos();
        int latePid = Nat.fork();
        if (latePid == 0) {
            int src = Nat.spawnSandboxed(99);
            if (src != 0) {
                System.out.println("sandbox_spawn_failed tag=99 rc=" + src);
                System.out.flush();
            }
            ZygoteHooks.postForkChild(0, false, false, null);
            Work.run(99, "forked-late", lateFork);
            System.exit(0);
        }
        ZygoteHooks.postForkCommon();
        say("late_fork child=" + latePid + " threads=" + Work.threadsInProc());
        say("late_fork_reaped child=" + latePid + " " + waited(Nat.waitPid(latePid)));
        say("PARENT_DONE total_ms=" + (Nat.nanos() - t0) / 1000000L);
    }

    /**
     * ZygoteInit.preloadClasses, reduced: every class name in the image's
     * {@code /system/etc/preloaded-classes} list is resolved once, here, so all
     * children inherit the resolved ArtMethod/ArtField/Class tables through CoW.
     * Resolution failures are counted, not fatal (ZygoteInit logs and continues).
     */
    private static void preload() throws Exception {
        BufferedReader in = new BufferedReader(new FileReader("/system/etc/preloaded-classes"));
        try {
            String line;
            int seen = 0;
            int shown = 0;
            while ((line = in.readLine()) != null) {
                line = line.trim();
                if (line.isEmpty() || line.startsWith("#")) {
                    continue;
                }
                if (seen >= preloadLimit) {
                    break;
                }
                seen++;
                try {
                    Class.forName(line, false, null);
                    preloadOk++;
                } catch (Throwable t) {
                    preloadFail++;
                    if (shown++ < 10) {
                        say("preload_fail class=" + line + " err=" + t);
                    }
                }
            }
            say("preload_considered=" + seen + " limit=" + preloadLimit);
        } finally {
            in.close();
        }
    }

    /** wait4's (ret, status) as an operator-readable line. */
    private static String waited(long v) {
        int ret = (int) (v >> 32);
        int st = (int) v;
        if (ret <= 0) {
            return "wait4_ret=" + ret + " errno=" + (-ret) + " (failed)"
                    + " status=0x" + Integer.toHexString(st);
        }
        int sig = st & 0x7f;
        if (sig != 0) {
            return "pid=" + ret + " terminated_by_signal=" + sig
                    + " core=" + ((st >> 7) & 1) + " status=0x" + Integer.toHexString(st);
        }
        return "pid=" + ret + " exit=" + ((st >> 8) & 0xff)
                + " status=0x" + Integer.toHexString(st);
    }

    private static String join(int[] v) {
        StringBuilder sb = new StringBuilder();
        for (int i = 0; i < v.length; i++) {
            sb.append(i == 0 ? "" : ",").append(v[i]);
        }
        return sb.toString();
    }

    private static void say(String s) {
        System.out.println(s);
        System.out.flush();
    }
}
