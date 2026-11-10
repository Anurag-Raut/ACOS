public class Main {
    static void p(String label, int v) {
        System.out.println(label + "=" + v);
    }

    public static void main(String[] args) {
        // Derive the operands from a runtime value so the compiler does not
        // constant-fold every arithmetic instruction away. With no args they
        // are exactly 17 and 5.
        int a = 17 + args.length;
        int b = 5 + args.length;
        p("add", a + b);
        p("sub", a - b);
        p("mul", a * b);
        p("div", a / b);
        p("rem", a % b);
        p("neg_div", (-a) / b);
        p("neg_rem", (-a) % b);
        p("div_neg", a / (-b));
        p("rem_neg", a % (-b));
        p("shl", a << b);
        p("shr", (-100 + args.length) >> 3);
        p("ushr", (-100 + args.length) >>> 3);
        p("and", a & b);
        p("or", a | b);
        p("xor", a ^ b);
        p("max", Integer.MAX_VALUE);
        p("min", Integer.MIN_VALUE);
        p("overflow_add", Integer.MAX_VALUE + 1);
        p("overflow_sub", Integer.MIN_VALUE - 1);
        p("overflow_mul", Integer.MAX_VALUE * 2);
        p("div_edge", Integer.MIN_VALUE / -1);
        p("rem_edge", Integer.MIN_VALUE % -1);
        p("rotate_left", Integer.rotateLeft(1, 4));
        p("rotate_right", Integer.rotateRight(-1, 4));
        p("bit_count", Integer.bitCount(0xF0F0));
        p("leading_zeros", Integer.numberOfLeadingZeros(1));
        p("trailing_zeros", Integer.numberOfTrailingZeros(8));
        p("highest_one_bit", Integer.highestOneBit(100));
        p("lowest_one_bit", Integer.lowestOneBit(100));
        p("signum", Integer.signum(-7));
    }
}
