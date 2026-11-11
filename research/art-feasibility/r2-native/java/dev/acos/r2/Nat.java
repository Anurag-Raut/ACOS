package dev.acos.r2;

/** JNI bridge to native/r2shim.c (see the header comment there). */
public final class Nat {
    private Nat() {}

    /** Load the shim. It is also LD_PRELOADed, so this resolves to that copy. */
    public static void load() {
        System.load("/data/lib/libr2shim.so");
    }

    /** Call an exported android::register_*(_JNIEnv*) entry point of libandroid_runtime.so. */
    public static native int register(String plainName);
}
