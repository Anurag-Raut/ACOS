package dev.acos.z1;

import android.os.Process;
import android.system.Os;
import android.system.OsConstants;
import java.io.BufferedReader;
import java.io.FileReader;

/**
 * Z1: the work every child process does, and the cold-start control.
 *
 * Identical code runs (a) in a forked zygote child and (b) as the main class of
 * a plain {@code dalvikvm64} process, so the two start costs are comparable:
 * allocate ~20 MiB, force 5 full GCs, start/join 4 threads that contend on one
 * monitor, report identity + private/inherited statics.
 */
public final class Work {
    public static final Object LOCK = new Object();
    public static final int ALLOC_MIB = 20;
    public static final int FULL_GCS = 5;
    public static final int NTHREADS = 4;
    public static final int ITERS = 20000;

    /** Written by the parent before any fork: every child must still see it. */
    public static int INHERITED = -1;
    /** Written by each child after the fork: siblings must disagree. */
    public static int PRIVATE = -1;
    /** Incremented under {@link #LOCK} by the child's threads. */
    public static int CONTENDED = 0;

    private static byte[][] heap;
    private static long checksum;

    private Work() {}

    public static void allocate(int tag) {
        heap = new byte[ALLOC_MIB][];
        long sum = 0;
        for (int i = 0; i < ALLOC_MIB; i++) {
            byte[] b = new byte[1024 * 1024];
            b[0] = (byte) tag;
            b[b.length - 1] = (byte) tag;
            heap[i] = b;
            sum += b.length;
        }
        checksum = sum;
    }

    /** Runtime.gc() is an explicit full GC (kGcCauseExplicit, non-concurrent). */
    public static long[] fullGcs(boolean withFinalize) {
        long[] ms = new long[FULL_GCS];
        for (int i = 0; i < FULL_GCS; i++) {
            System.out.println("gc_step " + Nat.getpid() + " begin " + i);
            System.out.flush();
            long t = Nat.nanos();
            Runtime.getRuntime().gc();
            System.out.println("gc_step " + Nat.getpid() + " after_gc " + i
                    + " ms=" + (Nat.nanos() - t) / 1000000L);
            System.out.flush();
            if (withFinalize) {
                System.runFinalization();
            }
            System.out.println("gc_step " + Nat.getpid() + " after_fin " + i
                    + " finalize=" + withFinalize);
            System.out.flush();
            ms[i] = (Nat.nanos() - t) / 1000000L;
        }
        return ms;
    }

    /** 4 threads, one monitor, no sleep: pure lock contention. */
    public static long threads() throws Exception {
        CONTENDED = 0;
        Thread[] ts = new Thread[NTHREADS];
        for (int i = 0; i < NTHREADS; i++) {
            final int id = i;
            ts[i] = new Thread(new Runnable() {
                public void run() {
                    for (int k = 0; k < ITERS; k++) {
                        synchronized (LOCK) {
                            CONTENDED++;
                            if (CONTENDED % 4096 == 0) {
                                Thread.yield();
                            }
                        }
                    }
                    if (id < 0) {
                        throw new AssertionError();
                    }
                }
            }, "z1-worker-" + i);
            ts[i].start();
        }
        for (int i = 0; i < NTHREADS; i++) {
            ts[i].join();
        }
        return (long) NTHREADS * ITERS;
    }

    /** Heap facts that must stay private per process. */
    public static String heapLine() {
        Runtime r = Runtime.getRuntime();
        return "heap_used_kb=" + ((r.totalMemory() - r.freeMemory()) >> 10)
                + " heap_total_kb=" + (r.totalMemory() >> 10)
                + " max_kb=" + (r.maxMemory() >> 10);
    }

    /**
     * Runtime::StartDaemonThreads (runtime.cc:1350) -- the only caller of
     * java.lang.Daemons.start(), i.e. of ReferenceQueueDaemon, FinalizerDaemon,
     * FinalizerWatchdogDaemon, HeapTaskDaemon and the JIT control thread -- is
     * invoked from Runtime::Start (runtime.cc:1127), which a zygote never runs.
     * A forked child therefore has no finalizer daemon until someone starts it;
     * this is the one "extra" step the prototype needs, and it is still
     * Google's code (java.lang.Daemons.start), not a re-implementation.
     */
    public static boolean startDaemons() {
        try {
            Class<?> c = Class.forName("java.lang.Daemons");
            java.lang.reflect.Method m = c.getDeclaredMethod("start");
            m.setAccessible(true);
            m.invoke(null);
            return true;
        } catch (java.lang.reflect.InvocationTargetException e) {
            // Already started (a non-zygote cold start did it in Runtime::Start).
            return !(e.getCause() instanceof IllegalStateException);
        } catch (Throwable t) {
            System.out.println("daemons_start_failed pid=" + Nat.getpid() + " err=" + t);
            return false;
        }
    }

    /** ART logs every explicit GC itself; counting them here would need
     * android.os.Debug.getRuntimeStat, which aborts on this sysroot. */
    public static String gcStat() {
        return "gc_requested=" + FULL_GCS;
    }

    public static int threadsInProc() {
        return threadCount();
    }

    static int threadCount() {
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
        } catch (Throwable t) {
            return -1;
        }
        return -1;
    }

    static long startJiffies() {
        try {
            String s = new String(readAll("/proc/self/stat"));
            int close = s.lastIndexOf(')');
            String[] f = s.substring(close + 2).split(" ");
            // field 22 of /proc/PID/stat (1-based); after ") " we are at field 3
            return Long.parseLong(f[19]);
        } catch (Throwable t) {
            return -1;
        }
    }

    static byte[] readAll(String path) throws Exception {
        java.io.FileInputStream in = new java.io.FileInputStream(path);
        try {
            byte[] b = new byte[4096];
            int n = in.read(b);
            byte[] out = new byte[n < 0 ? 0 : n];
            if (n > 0) {
                System.arraycopy(b, 0, out, 0, n);
            }
            return out;
        } finally {
            in.close();
        }
    }

    /**
     * The whole body, printed in one line per stage. `mode` is "forked" or
     * "cold"; `tag` is the child's identity.
     */
    public static void run(int tag, String mode, long forkNs) throws Exception {
        PRIVATE = tag * 1000 + Nat.getpid();
        int pid = Nat.getpid();
        boolean daemons = startDaemons();
        System.out.println("work mode=" + mode + " pid=" + pid + " tag=" + tag
                + " inherited=" + INHERITED + " private=" + PRIVATE
                + " ihash_String=" + System.identityHashCode(String.class)
                + " ihash_Context=" + System.identityHashCode(android.content.Context.class)
                + " threads=" + threadCount());
        long t0 = Nat.nanos();
        allocate(tag);
        System.out.println("alloc pid=" + pid + " mib=" + ALLOC_MIB
                + " bytes=" + checksum + " ms=" + (Nat.nanos() - t0) / 1000000L);
        long[] gcs = fullGcs(daemons);
        StringBuilder sb = new StringBuilder();
        for (int i = 0; i < gcs.length; i++) {
            sb.append(i == 0 ? "" : ",").append(gcs[i]);
        }
        System.out.println("gc pid=" + pid + " n=" + FULL_GCS + " ms=[" + sb + "] "
                + gcStat() + " " + heapLine() + " threads=" + threadCount());
        long t1 = Nat.nanos();
        threads();
        System.out.println("threads pid=" + pid + " n=" + NTHREADS + " contended=" + CONTENDED
                + " expected=" + ((long) NTHREADS * ITERS)
                + " ms=" + (Nat.nanos() - t1) / 1000000L + " threads=" + threadCount());
        System.out.println("ready pid=" + pid + " tag=" + tag + " mode=" + mode
                + " inherited=" + INHERITED + " private=" + PRIVATE
                + " fork_to_ready_ms=" + (Nat.nanos() - forkNs) / 1000000L
                + " threads=" + threadCount());
        System.out.flush();
    }

    /** Cold control: a plain dalvikvm64 startup running exactly the same work. */
    public static void main(String[] args) throws Exception {
        int tag = args.length > 0 ? Integer.parseInt(args[0]) : 7;
        long startNs = startJiffies() * 1000000000L / Os.sysconf(OsConstants._SC_CLK_TCK);
        System.out.println("work mode=main pid=" + Nat.getpid() + " tag=" + tag
                + " inherited=" + INHERITED
                + " ihash_String=" + System.identityHashCode(String.class)
                + " ihash_Context=" + System.identityHashCode(android.content.Context.class)
                + " threads=" + threadCount());
        System.out.flush();
        if (args.length > 1 && args[1].equals("waittest")) {
            System.out.println("waittest sigchld rc=" + Nat.resetSigchld());
            int p = Nat.fork();
            if (p == 0) {
                System.out.println("waittest child pid=" + Nat.getpid()
                        + " ppid=" + Nat.getppid() + " exiting 3");
                System.out.flush();
                System.exit(3);
            }
            Thread.sleep(300);
            long v = Nat.waitPid(p);
            long any = Nat.waitPid(-1);
            System.out.println("waittest parent pid=" + Nat.getpid() + " forked=" + p
                    + " wait4_ret=" + (int) (v >> 32) + " status=0x"
                    + Integer.toHexString((int) v)
                    + " wait4_any_ret=" + (int) (any >> 32)
                    + " any_status=0x" + Integer.toHexString((int) any));
            System.out.flush();
            Thread.sleep(500);
            System.exit(0);
        }
        boolean cold = args.length > 1 && args[1].equals("cold");
        long vmNs = Nat.nanos();
        run(tag, cold ? "cold" : "main", vmNs);
        if (cold) {
            long ready = Nat.nanos();
            System.out.println("COLD_READY pid=" + Nat.getpid()
                    + " exec_to_main_ms=" + (vmNs - startNs) / 1000000L
                    + " exec_to_ready_ms=" + (ready - startNs) / 1000000L
                    + " work_ms=" + (ready - vmNs) / 1000000L);
            System.out.flush();
            Thread.sleep(2000);
        }
    }
}
