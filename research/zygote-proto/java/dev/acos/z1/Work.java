package dev.acos.z1;

import android.system.ErrnoException;
import android.system.Os;
import android.system.OsConstants;
import java.io.BufferedReader;
import java.io.File;
import java.io.FileDescriptor;
import java.io.FileInputStream;
import java.io.FileOutputStream;
import java.io.FileReader;

/**
 * Z2: the work every child process does, the cold-start control, the sandbox
 * evidence, and the ECHILD diagnostic.
 *
 * Identical work runs (a) in a forked zygote child and (b) as the main class of
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

    private static final String[] RC_NAMES = {
        "userns", "mntns", "pidns", "mount_private", "tmpfs_data",
        "mkdir_private", "tmpfs_zygote", "proc", "seccomp", "capset",
        "setresgid", "setresuid_other", "setresuid", "uid_before", "gid_before",
        "pid_in_ns", "reaper_pid"
    };

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
     * java.lang.Daemons.start() -- is invoked from Runtime::Start, which a
     * zygote never runs.  A forked child therefore has no finalizer daemon
     * until someone starts it; this is Google's own code, not a reimplementation.
     */
    public static boolean startDaemons() {
        try {
            Class<?> c = Class.forName("java.lang.Daemons");
            java.lang.reflect.Method m = c.getDeclaredMethod("start");
            m.setAccessible(true);
            m.invoke(null);
            return true;
        } catch (java.lang.reflect.InvocationTargetException e) {
            return !(e.getCause() instanceof IllegalStateException);
        } catch (Throwable t) {
            System.out.println("daemons_start_failed pid=" + Nat.getpid() + " err=" + t);
            return false;
        }
    }

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

    // ---------------------------------------------------------------- Z2

    /** One line per /proc/self/status signal mask we care about. */
    public static String statusMask(String key) {
        try {
            BufferedReader in = new BufferedReader(new FileReader("/proc/self/status"));
            try {
                String line;
                while ((line = in.readLine()) != null) {
                    if (line.startsWith(key + ":")) {
                        return line.substring(key.length() + 1).trim();
                    }
                }
            } finally {
                in.close();
            }
        } catch (Throwable t) {
            return "read_err";
        }
        return "missing";
    }

    public static String nsId(String name) {
        try {
            return Os.readlink("/proc/self/ns/" + name);
        } catch (ErrnoException e) {
            return "errno=" + e.errno;
        } catch (Throwable t) {
            return "err=" + t;
        }
    }

    private static String nsLine() {
        return "mnt=" + nsId("mnt") + " pid=" + nsId("pid")
                + " user=" + nsId("user") + " net=" + nsId("net")
                + " ipc=" + nsId("ipc") + " uts=" + nsId("uts")
                + " cgroup=" + nsId("cgroup") + " time=" + nsId("time");
    }

    /** The zygote prints this before forking, so the child's ids can be compared. */
    public static void printParentNs(String tag) {
        System.out.println("parent_ns " + tag + " " + nsLine());
        System.out.flush();
    }

    private static boolean writeFile(String path, String data) {
        try {
            FileOutputStream f = new FileOutputStream(path);
            try {
                f.write(data.getBytes("UTF-8"));
            } finally {
                f.close();
            }
            return true;
        } catch (Throwable t) {
            System.out.println("iso_write_fail path=" + path + " err=" + t);
            System.out.flush();
            return false;
        }
    }

    public static String readProbe(String path) {
        try {
            FileDescriptor fd = Os.open(path, OsConstants.O_RDONLY, 0);
            try {
                byte[] b = new byte[256];
                int n = Os.read(fd, b, 0, b.length);
                return "read_n=" + n + " data="
                        + new String(b, 0, Math.max(n, 0), "UTF-8").trim();
            } finally {
                Os.close(fd);
            }
        } catch (ErrnoException e) {
            return "errno=" + e.errno + "(" + OsConstants.errnoName(e.errno) + ")";
        } catch (Throwable t) {
            return "err=" + t;
        }
    }

    /**
     * Per-child evidence: sandbox setup return codes, identity, namespace ids,
     * seccomp enforcement, and the isolation probes the brief asks for.
     */
    public static void sandboxEvidence(int tag) {
        StringBuilder sb = new StringBuilder("sandbox_rc");
        for (int i = 0; i < RC_NAMES.length; i++) {
            sb.append(' ').append(i).append(':').append(RC_NAMES[i])
                    .append('=').append(Nat.sandboxRc(i));
        }
        System.out.println(sb.toString());
        System.out.println("sandbox_ids pid=" + Nat.getpid() + " ppid=" + Nat.getppid()
                + " uid=" + Nat.getuid() + " euid=" + Nat.geteuid()
                + " gid=" + Nat.getgid() + " threads=" + threadCount());
        System.out.println("sandbox_ns " + nsLine());
        System.out.println("sandbox_seccomp status=" + Nat.seccompStatus()
                + " ptrace_rc=" + Nat.ptraceSelf()
                + " mount_rc=" + Nat.mountProbe()
                + " unshare_rc=" + Nat.unshareProbe()
                + " fork_rc=" + Nat.fork());
        System.out.flush();

        String myDir = "/data/data/pkg-" + tag;
        File d = new File(myDir);
        System.out.println("iso_private_dir path=" + myDir + " exists=" + d.exists()
                + " canWrite=" + d.canWrite());
        boolean wrote = writeFile(myDir + "/secret.txt", "tag=" + tag + " pid=" + Nat.getpid());
        System.out.println("iso_own_file wrote=" + wrote + " "
                + readProbe(myDir + "/secret.txt"));
        int sib = (tag == 1) ? 2 : 1;
        System.out.println("iso_sibling path=/data/data/pkg-" + sib + "/secret.txt "
                + readProbe("/data/data/pkg-" + sib + "/secret.txt"));
        System.out.println("iso_zygote path=/data/zygote-private/secret.txt "
                + readProbe("/data/zygote-private/secret.txt"));
        System.out.println("iso_sibling_dir_exists=" + new File("/data/data/pkg-" + sib).exists()
                + " iso_data_listing=" + java.util.Arrays.toString(new File("/data/data").list()));
        System.out.flush();
    }

    /** Z1 signature kept: the whole body, printed in one line per stage. */
    public static void run(int tag, String mode, long forkNs) throws Exception {
        PRIVATE = tag * 1000 + Nat.getpid();
        int pid = Nat.getpid();
        boolean daemons = startDaemons();
        System.out.println("work mode=" + mode + " pid=" + pid + " tag=" + tag
                + " inherited=" + INHERITED + " private=" + PRIVATE
                + " ihash_String=" + System.identityHashCode(String.class)
                + " ihash_Context=" + System.identityHashCode(android.content.Context.class)
                + " threads=" + threadCount());
        System.out.flush();
        if (mode.equals("forked") || mode.equals("forked-late")) {
            sandboxEvidence(tag);
        }
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

    private static String hex(long v) {
        return "0x" + Long.toHexString(v);
    }

    private static String wdesc(long v) {
        int ret = (int) (v >> 32);
        int st = (int) v;
        String s = "ret=" + ret + (ret < 0 ? "(errno=" + (-ret) + ")" : "")
                + " status=" + hex(st & 0xffffffffL);
        if (ret > 0) {
            s += " exit=" + ((st >> 8) & 0xff) + " sig=" + (st & 0x7f)
                    + " core=" + ((st >> 7) & 1);
        }
        return s;
    }

    /**
     * Z2 blocker #1: who makes wait4(2) return ECHILD inside ART?  Prints the
     * SIGCHLD disposition and every /proc/self/status mask at each step, and
     * waits on a live child (sleeps 1500 ms) both with WNOHANG and blocking.
     */
    public static void diag() throws Exception {
        System.out.println("diag_start pid=" + Nat.getpid() + " ppid=" + Nat.getppid()
                + " threads=" + threadCount());
        System.out.println("diag_masks before SigIgn=" + statusMask("SigIgn")
                + " SigCgt=" + statusMask("SigCgt") + " SigBlk=" + statusMask("SigBlk"));
        System.out.println("diag_sigchld before action=" + hex(Nat.sigchldAction()));
        int rc = Nat.resetSigchld();
        System.out.println("diag_reset rc=" + rc + " action_after=" + hex(Nat.sigchldAction()));
        System.out.println("diag_masks after_reset SigIgn=" + statusMask("SigIgn")
                + " SigCgt=" + statusMask("SigCgt") + " SigBlk=" + statusMask("SigBlk"));
        System.out.flush();

        int p = Nat.fork();
        if (p == 0) {
            System.out.println("diag_child pid=" + Nat.getpid() + " ppid=" + Nat.getppid()
                    + " SigIgn=" + statusMask("SigIgn") + " action="
                    + hex(Nat.sigchldAction()));
            System.out.flush();
            Thread.sleep(1500);
            System.out.println("diag_child exiting 3");
            System.out.flush();
            System.exit(3);
        }
        System.out.println("diag_parent forked=" + p + " immediately WNOHANG "
                + wdesc(Nat.waitPidOpt(p, 1 /* WNOHANG */)));
        Thread.sleep(300);
        System.out.println("diag_parent t+300ms WNOHANG "
                + wdesc(Nat.waitPidOpt(p, 1 /* WNOHANG */)));
        System.out.println("diag_masks parent_after_fork SigIgn=" + statusMask("SigIgn")
                + " SigCgt=" + statusMask("SigCgt") + " SigBlk=" + statusMask("SigBlk"));
        System.out.flush();
        System.out.println("diag_parent blocking_wait " + wdesc(Nat.waitPid(p)));
        System.out.println("diag_parent wait_any " + wdesc(Nat.waitPidOpt(-1, 0)));
        System.out.println("diag_parent WALL_WNOHANG "
                + wdesc(Nat.waitPidOpt(p, 0x40000000 | 1)));
        System.out.flush();
    }

    /** Cold control: a plain dalvikvm64 startup running exactly the same work. */
    public static void main(String[] args) throws Exception {
        int tag = args.length > 0 ? Integer.parseInt(args[0]) : 7;
        if (args.length > 1 && args[1].equals("diag")) {
            diag();
            return;
        }
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
