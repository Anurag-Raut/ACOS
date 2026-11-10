package sun.misc;

/**
 * Compile-time stub for the hidden `sun.misc.Unsafe` used by
 * {@code NativeMemMain}. The DEX under test references the real boot-classpath
 * class at runtime; this only lets {@code javac} resolve the methods and the
 * {@code theUnsafe} field.
 */
public final class Unsafe {
    private static final Unsafe theUnsafe = null;

    private Unsafe() {}

    public static Unsafe getUnsafe() {
        return theUnsafe;
    }

    public long allocateMemory(long bytes) {
        return 0;
    }

    public void freeMemory(long address) {}

    public long reallocateMemory(long address, long bytes) {
        return 0;
    }

    public void setMemory(long address, long bytes, byte value) {}

    public void copyMemory(long srcAddress, long destAddress, long bytes) {}

    public byte getByte(long address) {
        return 0;
    }

    public void putByte(long address, byte x) {}

    public short getShort(long address) {
        return 0;
    }

    public void putShort(long address, short x) {}

    public char getChar(long address) {
        return 0;
    }

    public void putChar(long address, char x) {}

    public int getInt(long address) {
        return 0;
    }

    public void putInt(long address, int x) {}

    public long getLong(long address) {
        return 0;
    }

    public void putLong(long address, long x) {}

    public float getFloat(long address) {
        return 0;
    }

    public void putFloat(long address, float x) {}

    public double getDouble(long address) {
        return 0;
    }

    public void putDouble(long address, double x) {}
}
