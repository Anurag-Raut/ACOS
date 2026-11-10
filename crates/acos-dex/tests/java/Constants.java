public class Constants {
    public static final int[] INTS = { 1, 2, 3, 5, 8, 13, 21, 34 };
    public static final long[] LONGS = { 1L, 1L << 32, -1L, 0x7fffffffffffffffL };
    public static final short[] SHORTS = { 1, 2, 3, 4 };
    public static final byte[] BYTES = { 0, 1, -1, 127, -128 };
    public static final char[] CHARS = { 'a', 'b', '\u00e9', '\u4e2d' };
    public static final boolean[] BOOLS = { true, false, true };
    public static final float[] FLOATS = { 0.0f, 1.5f, -2.5f };
    public static final double[] DOUBLES = { 0.0, 1.5, -2.5, Math.PI };
    public static final String GREETING = "h\u00e9llo \u4e2d\u6587 \uD83D\uDE00 \uD835\uDD6C";
    public static final String EMPTY = "";
    public static final String NUL = "a\u0000b";
    public static final int[][] MATRIX = { { 1, 2 }, { 3, 4 }, { 5, 6 } };
    public static final int MASK = 0xF0F0;
    public static final long BIG = 0x0123456789ABCDEFL;
    public static final double SMALL = 1.0e-10;
    public static final float F = 3.14159f;

    public static int sum() {
        int s = 0;
        for (int i : INTS) s += i;
        for (long l : LONGS) s += (int) l;
        for (short x : SHORTS) s += x;
        for (byte b : BYTES) s += b;
        for (char c : CHARS) s += c;
        for (boolean b : BOOLS) s += b ? 1 : 0;
        for (float f : FLOATS) s += (int) f;
        for (double d : DOUBLES) s += (int) d;
        for (int[] row : MATRIX) for (int v : row) s += v;
        return s + GREETING.length() + EMPTY.length() + NUL.length() + MASK + (int) BIG
                + (int) SMALL + (int) F;
    }

    public static void main(String[] args) {
        System.out.println(sum());
    }
}
