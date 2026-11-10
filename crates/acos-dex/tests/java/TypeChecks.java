public class TypeChecks {
    public static int classify(Object o) {
        if (o instanceof String) return 1;
        if (o instanceof Integer) return 2;
        if (o instanceof long[]) return 3;
        if (o instanceof int[][]) return 4;
        if (o == null) return 0;
        return -1;
    }

    public static String casts(Object o) {
        String s = (String) o;
        CharSequence cs = (CharSequence) o;
        return s + ":" + cs.length();
    }

    public static int multiDim(int n) {
        int[][][] cube = new int[n][n][n];
        int s = 0;
        for (int i = 0; i < n; i++) {
            for (int j = 0; j < n; j++) {
                for (int k = 0; k < n; k++) {
                    cube[i][j][k] = i + j + k;
                    s += cube[i][j][k];
                }
            }
        }
        return s;
    }

    public static int arrays(int n) {
        int[] a = new int[n];
        long[] b = new long[n];
        double[] c = new double[n];
        String[] d = new String[n];
        Object[] e = new Object[n];
        for (int i = 0; i < n; i++) {
            a[i] = i;
            b[i] = i;
            c[i] = i;
            d[i] = "s" + i;
            e[i] = d[i];
        }
        return a.length + b.length + c.length + d.length + e.length
                + (int) b[n - 1] + (int) c[n - 1] + d[n - 1].length();
    }

    public static void main(String[] args) {
        System.out.println(classify("x") + classify(1) + classify(new long[0])
                + classify(new int[0][0]) + classify(null) + classify(1.0)
                + casts("hello").length() + multiDim(3) + arrays(4));
    }
}
