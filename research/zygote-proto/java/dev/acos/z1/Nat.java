package dev.acos.z1;

/** Z2: the entire Java surface of the raw-syscall shim (see shim/z2fork.c). */
public final class Nat {
    static {
        System.load("/data/shim/libz2fork.so");
    }

    private Nat() {}

    public static native int fork();

    /** (wait4 return &lt;&lt; 32) | status */
    public static native long waitPid(int pid);

    /** Same encoding, with wait4 options (1 = WNOHANG, 0x40000000 = __WALL). */
    public static native long waitPidOpt(int pid, int options);

    public static native int getpid();

    public static native int getppid();

    public static native int getuid();

    public static native int getgid();

    public static native int geteuid();

    public static native int kill(int pid, int sig);

    /** rt_sigaction(SIGCHLD, SIG_DFL); returns 0 on success. */
    public static native int resetSigchld();

    /** (handler &lt;&lt; 32) | flags; handler 0 = SIG_DFL, 1 = SIG_IGN. */
    public static native long sigchldAction();

    public static native long nanos();

    // ---- Z2 sandbox ----

    /** unshare(CLONE_NEWUSER) + the one-line uid/gid map an unprivileged process may write. */
    public static native int setupUserNs();

    /** Slot from the sandbox setup record (see shim/z2fork.c). */
    public static native int sandboxRc(int slot);

    /**
     * Called in the zygote's forked child.  Returns 0 only in the sandboxed app
     * (PID 2 of a private PID namespace); the two intermediate processes exit.
     */
    public static native int spawnSandboxed(int tag);

    /**
     * Z4: Z2's sandbox topology for a session of a shared zygote.  The child
     * bind-mounts `/sessions/<tag>/binds` (one `src<TAB>dst` per line) in its
     * private mount namespace.  Returns 0 only in the app process (C).
     */
    public static native int spawnSession(int tag);

    /** Post-sandbox probes: each returns -errno, or a pid for fork(). */
    public static native int ptraceSelf();

    public static native int mountProbe();

    public static native int unshareProbe();

    /** prctl(PR_GET_SECCOMP): 0 = none, 1 = strict, 2 = filter. */
    public static native int seccompStatus();
}
