package dev.acos.z1;

/** Z1: the entire Java surface of the fork shim (see shim/z1fork.c). */
public final class Nat {
    static {
        System.load("/data/shim/libz1fork.so");
    }

    private Nat() {}

    public static native int fork();

    /** (wait4 return &lt;&lt; 32) | status */
    public static native long waitPid(int pid);

    public static native int getpid();

    public static native int getppid();

    /** rt_sigaction(SIGCHLD, SIG_DFL); returns 0 on success. */
    public static native int resetSigchld();

    public static native long nanos();
}
