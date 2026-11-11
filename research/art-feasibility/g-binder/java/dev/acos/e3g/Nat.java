package dev.acos.e3g;

/** JNI bridge into the E3G userspace binder driver (native/binder_driver.c). */
public final class Nat {
    private Nat() {}

    /** Load the shim (also preloaded by LD_PRELOAD; System.load is a no-op then). */
    public static void load() { System.load("/data/lib/libbindere3g.so"); }

    /** Call an exported android::register_*_*(_JNIEnv*) entry point. */
    public static native int register(String plainName);

    /** Attach a native thread to the VM and run IPCThreadState::joinThreadPool. */
    public static native int startBinderLooper();

    /** Driver state (transport, object tables, queue) for failure diagnosis. */
    public static native String state();
}
