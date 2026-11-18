package dev.acos.art;

/**
 * JNI bridge into the ACOS userspace binder driver
 * (crates/acos-binder-peer/native/binder_driver.c).
 *
 * <p>Moved here from the peer's test dex (`dev.acos.e3g.Nat`) so guest code
 * under {@code guest/java} never depends on another crate's test sources. The
 * driver exports both JNI names (`Java_dev_acos_art_Nat_*` forwards to the
 * original entry points), so the P2f acceptance dex keeps working unchanged.
 */
public final class Nat {
    private Nat() {}

    /** Load the driver (also preloaded by LD_PRELOAD; System.load is a no-op then). */
    public static void load() { System.load("/data/lib/libacos_binder_driver.so"); }

    /** Call an exported android::register_*_*(_JNIEnv*) entry point. */
    public static native int register(String plainName);

    /** Attach a native thread to the VM and run IPCThreadState::joinThreadPool. */
    public static native int startBinderLooper();

    /** Driver state (transport, object tables, queue) for failure diagnosis. */
    public static native String state();

    /**
     * R3: install the guest's native-crash handler.
     *
     * <p>Registered *last* in ART's own signal chain (libsigchain, after the
     * runtime's fault handlers), so it only runs on a fault ART declined — a
     * null deref outside generated code, a bad native pointer in app code, a
     * SIGABRT from a native CHECK. It prints one `ACOSCRASH {json}` line (fault
     * address, signal code, tid, thread name) and then asks the runtime for its
     * own crash dump (`Fatal signal` / regs / `Backtrace` with mapped library
     * paths). The handler never claims the signal: the process still dies of it.
     *
     * <p>Lives in the freestanding preload shim (`shim/artshim.c`), which is in
     * the process before libart is, so no APK or framework code is involved.
     *
     * @return 1 installed, 0 disabled (`ACOSART_CRASH_DUMP=0`), &lt;0 failed
     */
    public static native int installCrashHandler();

    /**
     * ZDEFAULT1: a zygote child's guest-log sink is re-resolved through its own
     * {@code /dev/socket} (the keeper's connection was inherited across the fork).
     *
     * @return 1 reset, 0 when no preload shim is in this process
     */
    public static native int resetLogSink();

    /** R3 test seam: 0 = SIGSEGV on an unmapped page, 1 = SIGABRT. */
    public static native int crashTest(int mode);
}
