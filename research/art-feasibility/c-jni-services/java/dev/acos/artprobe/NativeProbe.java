package dev.acos.artprobe;

/**
 * JNI natives under test. Nothing here is special: this is the ABI any Rust
 * service layer would have to satisfy on real ART.
 */
public final class NativeProbe {
    public static native int nativeAdd(int a, int b);

    public static native String nativeEcho(String in);

    public static native void nativeCallback(String msg);

    public static native int nativeCallbackFromThread();

    public static native int[] nativeCopyIntArray(int[] in);

    /** Transact against the stand-in ACOS ServiceHost (unix socket). */
    public static native String nativeServiceCall(String req);

    /** ns per round trip, measured inside native code over one connection. */
    public static native long nativeServiceBench(int n, String req);

    /** Fork n children off this warm process; pids written into `pids`. */
    public static native int nativeFork(int n, int holdMs, int[] pids);
}
