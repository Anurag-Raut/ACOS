package libcore.io;

/**
 * Compile-time stub for the hidden `libcore.io.Memory` used by
 * {@code NativeMemMain}. The DEX under test references the real boot-classpath
 * class at runtime.
 */
public final class Memory {
    private Memory() {}

    public static byte peekByte(long address) {
        return 0;
    }

    public static void pokeByte(long address, byte value) {}

    public static int peekInt(long address, boolean swap) {
        return 0;
    }

    public static void pokeInt(long address, int value, boolean swap) {}

    public static void peekByteArray(long address, byte[] dst, int dstOffset, int byteCount) {}

    public static void pokeByteArray(long address, byte[] src, int srcOffset, int byteCount) {}
}
