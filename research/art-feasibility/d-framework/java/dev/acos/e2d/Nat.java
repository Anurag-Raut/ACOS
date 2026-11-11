package dev.acos.e2d;

/** Thin JNI bridge into the "framework substrate" experiment (see native/shim.cpp). */
public final class Nat {
    private Nat() {}

    /** Load our own shim (links to nothing but libc/libdl). */
    public static void load() { System.load("/data/lib/libbindershim.so"); }

    /** Look up an exported symbol in an already-loaded shared object. */
    public static native long sym(String plainName);

    /**
     * Resolve and call an exported {@code android::register_*_*(_JNIEnv*)} entry
     * point of libandroid_runtime.so.  Returns 1 on success, 0 if the symbol is
     * missing, -1 if the call threw a Java exception, -2 if it crashed the
     * process (never observed).
     */
    public static native int register(String plainName);

    /** Raw kernel-level probe of the binder char devices. Multi-line report. */
    public static native String binderProbe();
}
