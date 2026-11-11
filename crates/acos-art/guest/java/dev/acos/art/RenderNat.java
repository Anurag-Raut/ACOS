package dev.acos.art;

/**
 * JNI bridge into the ACOS ART substrate shim
 * (crates/acos-art/shim/render_shim.c, LD_PRELOAD-ed as
 * {@code /data/app/libacosart.so}).
 *
 * The same object also dlopens libandroid_runtime.so + libhwui.so into the
 * *default* linker namespace in its constructor, which is what makes the
 * framework's exported {@code android::register_*_JNIEnv} entry points
 * reachable at all (see results/art-feasibility/E2D.md).
 */
public final class RenderNat {
    private RenderNat() {}

    /** Load the substrate shim (already LD_PRELOADed; this binds JNI lookup). */
    public static void load() { System.load("/data/app/libacosart.so"); }

    /**
     * Resolve and call an exported {@code android::register_*_*(_JNIEnv*)} entry
     * point. Returns 1 on success, 0 if the symbol is missing, -1 if the call
     * left a Java exception pending.
     */
    public static native int register(String plainName);

    /** Resolve an exported symbol, for diagnostics. */
    public static native long sym(String plainName);
}
