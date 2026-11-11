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
}
