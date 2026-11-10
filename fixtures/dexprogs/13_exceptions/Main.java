public class Main {
    static class MyException extends Exception {
        MyException(String msg) {
            super(msg);
        }
    }

    static class MyRuntime extends RuntimeException {
        MyRuntime(String msg) {
            super(msg);
        }
    }

    static int riskyDiv(int a, int b) {
        return a / b;
    }

    static String riskyNull() {
        String s = null;
        return s.toUpperCase();
    }

    static int riskyIndex(int[] a) {
        return a[10];
    }

    static Object riskyCast(Object o) {
        return (String) o;
    }

    static int riskySize(int n) {
        return new int[n].length;
    }

    static void tryFinally(String tag, boolean throwIt) {
        try {
            System.out.println(tag + ".try");
            if (throwIt) throw new MyException(tag + "-boom");
            System.out.println(tag + ".no-throw");
        } catch (MyException e) {
            System.out.println(tag + ".catch=" + e.getMessage());
        } finally {
            System.out.println(tag + ".finally");
        }
    }

    static int nested() {
        try {
            try {
                throw new MyRuntime("inner");
            } finally {
                System.out.println("inner.finally");
            }
        } catch (RuntimeException e) {
            System.out.println("outer.catch=" + e.getMessage());
            return 42;
        } finally {
            System.out.println("outer.finally");
        }
    }

    public static void main(String[] args) {
        tryFinally("a", false);
        tryFinally("b", true);
        System.out.println("nested=" + nested());
        int[][] cases = {{10, 2}, {10, 0}};
        for (int[] c : cases) {
            try {
                System.out.println("div=" + riskyDiv(c[0], c[1]));
            } catch (ArithmeticException e) {
                System.out.println("caught=" + e.getClass().getName());
            }
        }
        try {
            riskyNull();
        } catch (NullPointerException e) {
            System.out.println("caught=" + e.getClass().getName());
        }
        try {
            riskyIndex(new int[3]);
        } catch (ArrayIndexOutOfBoundsException e) {
            System.out.println("caught=" + e.getClass().getName());
        }
        try {
            riskyCast(Integer.valueOf(5));
        } catch (ClassCastException e) {
            System.out.println("caught=" + e.getClass().getName());
        }
        try {
            riskySize(-1);
        } catch (NegativeArraySizeException e) {
            System.out.println("caught=" + e.getClass().getName());
        }
        try {
            throw new MyRuntime("custom-runtime");
        } catch (RuntimeException e) {
            System.out.println("caught=" + e.getClass().getName() + " msg=" + e.getMessage());
        }
        try {
            throw new MyException("custom-checked");
        } catch (Exception e) {
            System.out.println("caught=" + e.getClass().getName() + " msg=" + e.getMessage());
        }
        try {
            try {
                throw new MyException("rethrow");
            } catch (MyException e) {
                throw new MyRuntime("wrapped:" + e.getMessage());
            }
        } catch (RuntimeException e) {
            System.out.println("wrapped=" + e.getMessage());
        }
        System.out.println("done");
    }
}
