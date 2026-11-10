public class Main {
    public static void main(String[] args) {
        // Runtime-derived operands so float/double arithmetic, conversions and
        // compares are actually emitted (and not constant-folded). With no args
        // zero is 0.0 and d/e/f keep their intended values.
        double zero = args.length;
        double d = 3.5 + zero;
        double e = -2.25 + zero;
        double nan = (d - d) / (e - e);
        double inf = d / zero;
        double ninf = (-d) / zero;
        float f = 1.5f + (float) zero;

        System.out.println("add=" + (d + e));
        System.out.println("sub=" + (d - e));
        System.out.println("mul=" + (d * e));
        System.out.println("div=" + (d / e));
        System.out.println("rem=" + (d % e));
        System.out.println("nan=" + nan);
        System.out.println("inf=" + inf);
        System.out.println("ninf=" + ninf);
        System.out.println("nan_eq=" + (nan == nan));
        System.out.println("nan_ne=" + (nan != nan));
        System.out.println("negzero=" + (-0.0));
        System.out.println("d2i_nan=" + (int) nan);
        System.out.println("d2i_posinf=" + (int) inf);
        System.out.println("d2i_neginf=" + (int) ninf);
        System.out.println("d2i_huge=" + (int) 1e30);
        System.out.println("d2l_huge=" + (long) 1e30);
        System.out.println("d2i_neg_huge=" + (int) -1e30);
        System.out.println("d2i_trunc=" + (int) -3.99);
        System.out.println("d2i_rt=" + (int) d);
        System.out.println("d2l_rt=" + (long) d);
        System.out.println("d2f_rt=" + (float) d);
        System.out.println("f2d_rt=" + (double) f);
        System.out.println("l2d_rt=" + (double) (long) d);
        System.out.println("i2d_max=" + (double) Integer.MAX_VALUE);
        System.out.println("d2l_max=" + (long) Double.MAX_VALUE);
        System.out.println("f2d=" + (double) 0.1f);
        System.out.println("d2f=" + (float) 0.1d);
        System.out.println("d2f_huge=" + (float) 1e300);
        System.out.println("fadd=" + (f + 2.25f));
        System.out.println("fcmp=" + (f < 3.0f ? "lt" : "ge"));
        System.out.println("nan_cmpl=" + (nan < 1.0 ? "lt" : "ge"));
        System.out.println("nan_cmpg=" + (nan > 1.0 ? "gt" : "le"));
        System.out.println("min=" + Math.min(1.0, 2.0));
        System.out.println("max_negzero=" + Math.max(-0.0, 0.0));
        System.out.println("sqrt2=" + Math.sqrt(2.0));
        System.out.println("abs=" + Math.abs(-3.5));
        System.out.println("floor=" + Math.floor(-2.5));
        System.out.println("ceil=" + Math.ceil(-2.5));
        System.out.println("round=" + Math.round(2.5));
        System.out.println("round_neg=" + Math.round(-2.5));
        System.out.println("pow=" + Math.pow(2.0, 10.0));
    }
}
