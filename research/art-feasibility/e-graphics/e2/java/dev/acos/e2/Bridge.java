package dev.acos.e2;

public final class Bridge {
    public static native int nativeCount();
    public static native int register(String cls, String name, String sig);
    public static native String[] classNames();
    public static native int installAll();
    public static native int registerReal();
}
