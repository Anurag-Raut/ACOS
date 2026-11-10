public class Arithmetic {
    public static long addLong(long a, long b) { return a + b; }
    public static long mulLong(long a, long b) { return a * b; }
    public static double addDouble(double a, double b) { return a + b; }
    public static int longToInt(long a) { return (int) a; }
    public static double longToDouble(long a) { return (double) a; }
    public static long doubleToLong(double a) { return (long) a; }
    public static int cmp(long a, long b) { return a < b ? -1 : (a == b ? 0 : 1); }
    public static double cmpDouble(double a, double b) {
        if (a < b) return -1.0;
        if (a > b) return 1.0;
        return 0.0;
    }
    public static int shifts(long a, int s) {
        return (int) (a >> s) | (int) (a >>> s) | (int) (a << s);
    }
    public static int mix(int a, int b) {
        return ((a + b) * 3 - (a ^ b)) / 2 % 7;
    }
    public static long wide2addr(long a, int b) {
        a += b;
        a *= 2;
        a >>= 1;
        a -= 3;
        a |= 1L;
        return a;
    }
    public static float floatOps(float a, float b) {
        return ((a + b) * (a - b)) / (a % b);
    }
    public static double doubleOps(double a, double b) {
        return ((a * b) + (a / b)) - (a % b);
    }
    public static void main(String[] args) {
        System.out.println(addLong(1, 2L)
                + mulLong(3, 4)
                + (long) addDouble(1.5, 2.5)
                + longToInt(9)
                + (long) longToDouble(3)
                + doubleToLong(4.5)
                + cmp(1, 2)
                + (int) cmpDouble(1.0, 2.0)
                + shifts(8L, 2)
                + mix(3, 4)
                + wide2addr(5L, 6)
                + (long) floatOps(1.0f, 2.0f)
                + (long) doubleOps(1.0, 2.0));
    }
}
