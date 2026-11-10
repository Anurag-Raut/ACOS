public class Main {
    static long fib(int n) {
        return n < 2 ? n : fib(n - 1) + fib(n - 2);
    }

    static long fact(int n) {
        return n <= 1 ? 1 : n * fact(n - 1);
    }

    static int gcd(int a, int b) {
        return b == 0 ? a : gcd(b, a % b);
    }

    static int ack(int m, int n) {
        if (m == 0) return n + 1;
        if (n == 0) return ack(m - 1, 1);
        return ack(m - 1, ack(m, n - 1));
    }

    static boolean isEven(int n) {
        return n == 0 ? true : isOdd(n - 1);
    }

    static boolean isOdd(int n) {
        return n == 0 ? false : isEven(n - 1);
    }

    static int sumTo(int n) {
        if (n == 0) return 0;
        return n + sumTo(n - 1);
    }

    static int power(int base, int exp) {
        if (exp == 0) return 1;
        return base * power(base, exp - 1);
    }

    public static void main(String[] args) {
        for (int i = 0; i <= 10; i++) {
            System.out.println("fib" + i + "=" + fib(i));
        }
        for (int i = 0; i <= 10; i++) {
            System.out.println("fact" + i + "=" + fact(i));
        }
        System.out.println("gcd(48,18)=" + gcd(48, 18));
        System.out.println("gcd(17,5)=" + gcd(17, 5));
        System.out.println("gcd(0,9)=" + gcd(0, 9));
        System.out.println("ack(2,3)=" + ack(2, 3));
        System.out.println("ack(3,3)=" + ack(3, 3));
        System.out.println("isEven(10)=" + isEven(10));
        System.out.println("isOdd(7)=" + isOdd(7));
        System.out.println("sumTo(100)=" + sumTo(100));
        System.out.println("power(2,16)=" + power(2, 16));
        System.out.println("power(3,7)=" + power(3, 7));
    }
}
