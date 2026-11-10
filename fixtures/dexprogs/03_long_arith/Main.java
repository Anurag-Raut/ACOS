public class Main {
    static void p(String label, long v) {
        System.out.println(label + "=" + v);
    }

    static void ps(String label, String v) {
        System.out.println(label + "=" + v);
    }

    public static void main(String[] args) {
        // Runtime operand so the long arithmetic opcodes are actually emitted.
        long a = 1234567890123L + args.length;
        long b = 987654321L + args.length;
        p("add", a + b);
        p("sub", a - b);
        p("mul", a * b);
        p("div", a / b);
        p("rem", a % b);
        p("shl", (1L + args.length) << 40);
        p("shr", (-1L + args.length) >> 3);
        p("ushr", (-1L + args.length) >>> 3);
        p("l2i", (int) 0x1_0000_0001L);
        p("i2l", (long) Integer.MIN_VALUE);
        p("max", Long.MAX_VALUE);
        p("min", Long.MIN_VALUE);
        p("overflow_add", Long.MAX_VALUE + 1);
        p("overflow_mul", Long.MAX_VALUE * 2);
        p("div_edge", Long.MIN_VALUE / -1);
        p("rem_edge", Long.MIN_VALUE % -1);
        p("bit_count", Long.bitCount(0xF0F0F0F0F0F0F0F0L));
        p("rotate_left", Long.rotateLeft(1L, 40));
        p("highest_one_bit", Long.highestOneBit(1L << 50));
        p("signum", Long.signum(-1234567890123L));
        p("compare", Long.compare(5L, 6L));
        p("parse_min", Long.parseLong("-9223372036854775808"));
        ps("to_string_radix", Long.toString(1234567890123L, 36));
        ps("to_hex", Long.toHexString(-1L));
        ps("to_octal", Long.toOctalString(64L));
        ps("to_binary", Long.toBinaryString(10L));
        p("narrow_short", (short) 0x1_2345L);
        p("narrow_byte", (byte) 200L);
        p("d2l_huge", (long) 1e18);
        p("f2l_nan", (long) Float.NaN);
    }
}
