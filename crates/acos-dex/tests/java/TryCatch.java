public class TryCatch {
    public static int simple(int x) {
        try {
            return 100 / x;
        } catch (ArithmeticException e) {
            return -1;
        }
    }

    public static int multi(int x) {
        try {
            int[] a = new int[3];
            a[x] = x;
            return a[x];
        } catch (ArrayIndexOutOfBoundsException e) {
            return -2;
        } catch (NullPointerException e) {
            return -3;
        } finally {
            System.out.println("finally " + x);
        }
    }

    public static int nested(int x) {
        try {
            try {
                if (x < 0) throw new IllegalStateException("neg");
                return x;
            } catch (IllegalStateException e) {
                return -4;
            } finally {
                if (x == 0) throw new RuntimeException("zero");
            }
        } catch (RuntimeException e) {
            return -5;
        }
    }

    public static int rethrow(int x) throws Exception {
        try {
            if (x == 0) throw new Exception("boom");
            return x;
        } catch (Exception e) {
            throw new Exception("wrapped", e);
        }
    }

    public static String tryWithResources(int x) {
        try (AutoCloseable c = () -> System.out.println("close")) {
            return "ok " + x;
        } catch (Exception e) {
            return "err";
        }
    }

    public static void main(String[] args) {
        try {
            System.out.println(simple(0) + multi(9) + nested(0) + rethrow(1) + tryWithResources(2));
        } catch (Exception e) {
            System.out.println(e);
        }
    }
}
