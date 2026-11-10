public class Varargs {
    public static int sum(int... xs) {
        int s = 0;
        for (int x : xs) s += x;
        return s;
    }

    public static String join(String sep, Object... parts) {
        StringBuilder sb = new StringBuilder();
        for (int i = 0; i < parts.length; i++) {
            if (i > 0) sb.append(sep);
            sb.append(parts[i]);
        }
        return sb.toString();
    }

    public static long mix(long base, int... xs) {
        long s = base;
        for (int x : xs) s += x;
        return s;
    }

    public static void main(String[] args) {
        System.out.println(sum(1, 2, 3, 4) + join("-", "a", "b", "c") + mix(100L, 1, 2, 3));
    }
}
